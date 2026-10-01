#![cfg(not(target_family = "wasm"))]
//! Authenticated platform ingress and outbound messaging for Rig.
//!
//! Platforms verify raw requests before normalizing events. [`Gateway`] applies
//! admission and duplicate detection before downloading media and running agents.
//!
//! ```
//! use rig_messaging_platforms::WebhookResponse;
//! assert_eq!(WebhookResponse::ack().status.as_u16(), 200);
//! ```

pub mod auth;
#[cfg(feature = "feishu")]
pub mod feishu;
pub mod gateway;
#[cfg(feature = "googlechat")]
pub mod googlechat;
pub mod http;
#[cfg(feature = "line")]
pub mod line;
#[cfg(feature = "lineworks")]
pub mod lineworks;
#[cfg(feature = "teams")]
pub mod teams;
#[cfg(feature = "telegram")]
pub mod telegram;
pub mod token;
#[cfg(feature = "wecom")]
pub mod wecom;

use ::http::{HeaderMap, Method, StatusCode};
use bytes::Bytes;
pub use gateway::Gateway;
pub use http::Http;
use rig_core::wasm_compat::WasmBoxedFuture;
use rig_messaging::{ChatAdapter, ChatError, Inbound};
use serde_json::Value;
use std::collections::HashMap;
pub use token::TokenCache;

/// Verified transport or platform failure. HTTP errors omit request URLs.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Authentication did not validate.
    #[error("platform authentication failed")]
    Authentication,
    /// Input violates the protocol contract.
    #[error("invalid platform input: {0}")]
    Invalid(&'static str),
    /// A URL-free transport failure.
    #[error("platform transport failed: {0}")]
    Http(#[source] rig_reqwest::reqwest::Error),
    /// JSON could not be parsed or serialized.
    #[error("invalid platform JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// An HTTP response was unsuccessful.
    #[error("platform HTTP status: {0}")]
    Status(StatusCode),
    /// A request or response exceeds the configured byte limit.
    #[error("platform payload exceeds byte limit")]
    TooLarge,
    /// A platform returned a protocol error.
    #[error("platform API failed ({code}): {message}")]
    Platform { code: String, message: String },
    /// Routing or delivery failed.
    #[error(transparent)]
    Chat(#[from] ChatError),
}
impl From<rig_reqwest::reqwest::Error> for Error {
    fn from(error: rig_reqwest::reqwest::Error) -> Self {
        Self::Http(error.without_url())
    }
}
impl From<Error> for ChatError {
    fn from(error: Error) -> Self {
        Self::Platform(Box::new(error))
    }
}

/// Raw bounded request, retained intact for signature verification.
#[derive(Debug)]
pub struct WebhookRequest {
    /// HTTP method.
    pub method: Method,
    /// Original HTTP headers.
    pub headers: HeaderMap,
    /// Decoded query parameters.
    pub query: HashMap<String, String>,
    /// Unmodified raw body.
    pub body: Bytes,
}
/// Normalized message plus provider metadata needed after admission.
#[derive(Debug, Clone)]
pub struct Incoming {
    /// Original message and reply identity.
    pub inbound: Inbound,
    /// Provider event metadata.
    pub payload: Value,
}
/// Platform acknowledgement or challenge and verified events.
#[derive(Debug)]
pub struct WebhookResponse {
    /// HTTP response status.
    pub status: StatusCode,
    /// Response MIME type.
    pub content_type: &'static str,
    /// Response body.
    pub body: Bytes,
    /// Verified normalized events.
    pub events: Vec<Incoming>,
}
impl WebhookResponse {
    /// Construct an empty successful acknowledgement.
    pub fn ack() -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "text/plain",
            body: Bytes::new(),
            events: Vec::new(),
        }
    }
}
/// Authenticated ingress paired with an outbound adapter.
pub trait Platform: ChatAdapter {
    /// Configured or platform-verified bot identity.
    fn bot_id(&self) -> &str;
    /// Verify the request before returning normalized events or a challenge.
    fn receive(
        &self,
        request: WebhookRequest,
    ) -> WasmBoxedFuture<'_, Result<WebhookResponse, Error>>;
    /// Scope a run with platform metadata belonging to this exact event.
    fn scope<'a>(
        &'a self,
        _event: Incoming,
        run: WasmBoxedFuture<'a, Result<(), Error>>,
    ) -> WasmBoxedFuture<'a, Result<(), Error>> {
        run
    }
    /// Download media and prepare reply metadata after admission.
    fn prepare(&self, event: Incoming) -> WasmBoxedFuture<'_, Result<Inbound, Error>>;
}
