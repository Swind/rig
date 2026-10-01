//! Webhook dispatch with bounded ingress, admission, and duplicate detection.
//!
//! ```
//! use rig_messaging_platforms::Gateway;
//! fn webhook(gateway: std::sync::Arc<Gateway>) -> axum::Router {
//!     gateway.route("/webhook")
//! }
//! ```
use crate::{Error, Incoming, Platform, WebhookRequest, WebhookResponse};
use ::http::StatusCode;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    response::Response,
    routing::any,
};
use rig_core::wasm_compat::{WasmBoxedFuture, WasmCompatSend, WasmCompatSync};
use rig_messaging::{ChannelRef, ChatRouter, Inbound};
use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
};
use tokio::sync::Mutex;

/// Application wrapper for per-conversation task-local context around router execution.
pub trait Dispatch: WasmCompatSend + WasmCompatSync {
    /// Run one prepared event within application context.
    fn dispatch(
        &self,
        router: Arc<ChatRouter>,
        platform: Arc<dyn Platform>,
        inbound: Inbound,
    ) -> WasmBoxedFuture<'static, Result<(), Error>>;
}
impl<F> Dispatch for F
where
    F: Fn(
            Arc<ChatRouter>,
            Arc<dyn Platform>,
            Inbound,
        ) -> WasmBoxedFuture<'static, Result<(), Error>>
        + WasmCompatSend
        + WasmCompatSync,
{
    fn dispatch(
        &self,
        router: Arc<ChatRouter>,
        platform: Arc<dyn Platform>,
        inbound: Inbound,
    ) -> WasmBoxedFuture<'static, Result<(), Error>> {
        self(router, platform, inbound)
    }
}

/// Shared router and platform webhook endpoint. Deduplication survives only this process.
pub struct Gateway {
    router: Arc<ChatRouter>,
    platform: Arc<dyn Platform>,
    dispatch: Arc<dyn Dispatch>,
    seen: Mutex<(
        HashSet<(ChannelRef, String)>,
        VecDeque<(ChannelRef, String)>,
    )>,
    dedup_limit: usize,
    body_limit: usize,
}
impl Gateway {
    /// Construct a gateway with bounded raw bodies and recent-message deduplication.
    pub fn new(
        router: Arc<ChatRouter>,
        platform: Arc<dyn Platform>,
        body_limit: usize,
        dedup_limit: usize,
    ) -> Result<Self, Error> {
        if body_limit == 0 || dedup_limit == 0 {
            return Err(Error::Invalid("gateway limits must be positive"));
        }
        Ok(Self {
            router,
            platform,
            body_limit,
            dedup_limit,
            seen: Mutex::new((HashSet::new(), VecDeque::new())),
            dispatch: Arc::new(
                |router: Arc<ChatRouter>,
                 platform: Arc<dyn Platform>,
                 inbound: Inbound|
                 -> WasmBoxedFuture<'static, Result<(), Error>> {
                    Box::pin(async move {
                        let bot_id = platform.bot_id().to_owned();
                        router.handle(platform, inbound, &bot_id).await?;
                        Ok(())
                    })
                },
            ),
        })
    }
    /// Wrap each router invocation, using the prepared inbound's reply session key.
    pub fn with_dispatch(mut self, dispatch: Arc<dyn Dispatch>) -> Self {
        self.dispatch = dispatch;
        self
    }
    /// Build an Axum route serving this platform at the supplied path.
    pub fn route(self: Arc<Self>, path: &str) -> Router {
        Router::new().route(path, any(handler)).with_state(self)
    }
    /// Verify a raw request and schedule admitted events without waiting for media or agents.
    pub async fn receive(
        self: &Arc<Self>,
        request: WebhookRequest,
    ) -> Result<WebhookResponse, Error> {
        if request.body.len() > self.body_limit {
            return Err(Error::TooLarge);
        }
        let mut response = self.platform.receive(request).await?;
        let events = std::mem::take(&mut response.events);
        if !events.is_empty() {
            let gateway = self.clone();
            tokio::spawn(async move {
                for event in events {
                    if let Err(error) = gateway.dispatch_event(event).await {
                        // Protocol errors can include remote text. Keep logs free of tokens.
                        tracing::warn!(kind = error_kind(&error), "messaging event failed");
                    }
                }
            });
        }
        Ok(response)
    }
    /// Admit and deduplicate one already verified event before preparing media.
    pub async fn dispatch_event(self: &Arc<Self>, event: Incoming) -> Result<(), Error> {
        if !self.router.allows(&event.inbound, self.platform.bot_id()) {
            return Ok(());
        }
        let identity = (
            event.inbound.message.channel.clone(),
            event.inbound.message.message_id.clone(),
        );
        {
            let mut seen = self.seen.lock().await;
            if !seen.0.insert(identity.clone()) {
                return Ok(());
            }
            seen.1.push_back(identity.clone());
            while seen.1.len() > self.dedup_limit {
                if let Some(old) = seen.1.pop_front() {
                    seen.0.remove(&old);
                }
            }
        }
        let payload = event.payload.clone();
        let inbound = match self.platform.prepare(event).await {
            Ok(inbound) => inbound,
            Err(error) => {
                let mut seen = self.seen.lock().await;
                seen.0.remove(&identity);
                seen.1.retain(|entry| entry != &identity);
                return Err(error);
            }
        };
        let dispatch = self.dispatch.clone();
        let router = self.router.clone();
        let platform = self.platform.clone();
        tokio::spawn(async move {
            let event = Incoming {
                inbound: inbound.clone(),
                payload,
            };
            let run = dispatch.dispatch(router, platform.clone(), inbound);
            if let Err(error) = platform.scope(event, run).await {
                tracing::warn!(kind = error_kind(&error), "messaging run failed");
            }
        });
        Ok(())
    }
}
fn error_kind(error: &Error) -> &'static str {
    match error {
        Error::Authentication => "authentication",
        Error::Invalid(_) => "invalid",
        Error::Http(_) => "transport",
        Error::Json(_) => "json",
        Error::Status(_) => "status",
        Error::TooLarge => "limit",
        Error::Platform { .. } => "platform",
        Error::Chat(_) => "chat",
    }
}
async fn handler(State(gateway): State<Arc<Gateway>>, request: axum::extract::Request) -> Response {
    let (parts, body) = request.into_parts();
    let result = async {
        let body = to_bytes(body, gateway.body_limit)
            .await
            .map_err(|_| Error::TooLarge)?;
        let query = parts
            .uri
            .query()
            .map(|query| rig_reqwest::reqwest::Url::parse(&format!("https://localhost/?{query}")))
            .transpose()
            .map_err(|_| Error::Invalid("query"))?
            .map(|url| url.query_pairs().into_owned().collect())
            .unwrap_or_default();
        gateway
            .receive(WebhookRequest {
                method: parts.method,
                headers: parts.headers,
                query,
                body,
            })
            .await
    }
    .await;
    let response = match result {
        Ok(response) => response,
        Err(error) => WebhookResponse {
            status: match error {
                Error::Authentication => StatusCode::UNAUTHORIZED,
                Error::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
                Error::Invalid(_) | Error::Json(_) => StatusCode::BAD_REQUEST,
                _ => StatusCode::BAD_GATEWAY,
            },
            content_type: "text/plain",
            body: bytes::Bytes::new(),
            events: Vec::new(),
        },
    };
    let mut output = Response::new(Body::from(response.body));
    *output.status_mut() = response.status;
    output.headers_mut().insert(
        ::http::header::CONTENT_TYPE,
        ::http::HeaderValue::from_static(response.content_type),
    );
    output
}
#[cfg(test)]
mod tests;
