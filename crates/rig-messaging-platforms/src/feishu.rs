//! Feishu and Lark bot messages, authenticated callbacks and long connections.
//!
//! Construct [`Feishu`] with application credentials and a verified bot identity.
//! Webhook verification and media preparation use separate ingress stages.
//!
//! ```no_run
//! # async fn example() -> Result<(), rig_messaging_platforms::Error> {
//! use rig_messaging_platforms::feishu::{Config, Feishu};
//! let bot = Feishu::connect(Config::new("app-id", "app-secret")).await?;
//! # Ok(()) }
//! ```

mod ingress;
mod websocket;
pub use websocket::{Frame, FrameHeader};

use crate::{Error, Http, Incoming, Platform, TokenCache, WebhookRequest, WebhookResponse};
use http::Method;
use rig_core::wasm_compat::WasmBoxedFuture;
use rig_messaging::{ChannelRef, ChatAdapter, ChatError, Inbound, MessageRef};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;

/// API region. Lark shares the Feishu wire protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Domain {
    /// Mainland China API.
    #[default]
    Feishu,
    /// International Lark API.
    Lark,
}

impl Domain {
    /// Official HTTPS API origin.
    pub fn api_base(self) -> &'static str {
        match self {
            Self::Feishu => "https://open.feishu.cn",
            Self::Lark => "https://open.larksuite.com",
        }
    }
}

/// Outbound text representation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Delivery {
    /// JSON 2.0 CardKit cards with native Markdown and uncapped preview updates.
    #[default]
    Card,
    /// Rich posts. Preview updates stop before the twenty-edit server limit.
    Post,
    /// Plain text messages.
    Text,
}

/// Application credentials and callback verification configuration.
/// Secrets are deliberately excluded from Debug output.
#[derive(Clone)]
pub struct Config {
    /// Application ID.
    pub app_id: String,
    /// Application secret.
    pub app_secret: String,
    /// API region.
    pub domain: Domain,
    /// Event verification token. Required for webhook mode.
    pub verification_token: Option<String>,
    /// Callback encryption/signature key. Required for webhook mode.
    pub encrypt_key: Option<String>,
    /// Reply representation.
    pub delivery: Delivery,
    /// API deadline per request.
    pub timeout: Duration,
}

impl Config {
    /// Create application configuration for the Feishu region.
    pub fn new(app_id: impl Into<String>, app_secret: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            app_secret: app_secret.into(),
            domain: Domain::Feishu,
            verification_token: None,
            encrypt_key: None,
            delivery: Delivery::Card,
            timeout: Duration::from_secs(15),
        }
    }
}

struct CardState {
    card_id: String,
    sequence: u64,
    finalized: bool,
}

/// Cloneable bot client. Clones share token and card state.
#[derive(Clone)]
pub struct Feishu {
    config: Config,
    http: Http,
    base: String,
    bot_id: String,
    tokens: Arc<TokenCache>,
    cards: Arc<Mutex<HashMap<String, Arc<Mutex<CardState>>>>>,
    edits: Arc<Mutex<HashMap<String, (Delivery, usize)>>>,
}

impl Feishu {
    /// Obtain tenant credentials and verify the bot's open ID through the API.
    pub async fn connect(config: Config) -> Result<Self, Error> {
        let base = config.domain.api_base().to_owned();
        Self::connect_at(config, base).await
    }

    async fn connect_at(config: Config, base: String) -> Result<Self, Error> {
        if config.app_id.trim().is_empty() || config.app_secret.trim().is_empty() {
            return Err(Error::Invalid(
                "Feishu application credentials are required",
            ));
        }
        let http = Http::new(config.timeout, 2 * 1024 * 1024)?;
        let mut client = Self {
            config,
            http,
            base,
            bot_id: String::new(),
            tokens: Arc::new(TokenCache::new()),
            cards: Arc::new(Mutex::new(HashMap::new())),
            edits: Arc::new(Mutex::new(HashMap::new())),
        };
        let identity = client
            .api(Method::GET, "/open-apis/bot/v3/info", None)
            .await?;
        client.bot_id = required(&identity, "/bot/open_id")?.to_owned();
        Ok(client)
    }

    async fn token(&self) -> Result<String, Error> {
        self.tokens.get_or_refresh(|| async {
            let value: Value = self.http.json(self.http.request(Method::POST,
                &format!("{}/open-apis/auth/v3/tenant_access_token/internal", self.base))
                .json(&json!({"app_id":self.config.app_id,"app_secret":self.config.app_secret}))).await?;
            check(&value)?;
            let token = required(&value, "/tenant_access_token")?.to_owned();
            let expires = value.pointer("/expire").unwrap_or(&Value::Null).as_u64().filter(|n| *n > 0)
                .ok_or(Error::Invalid("missing tenant token expiry"))?;
            Ok((token, Duration::from_secs(expires)))
        }).await
    }

    async fn api(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value, Error> {
        let mut refreshed = false;
        loop {
            let token = self.token().await?;
            let request = self
                .http
                .request(method.clone(), &format!("{}{}", self.base, path))
                .bearer_auth(token);
            let request = match body {
                Some(body) => request.json(body),
                None => request,
            };
            let result = self.http.json::<Value>(request).await;
            let expired = matches!(&result, Err(Error::Status(http::StatusCode::UNAUTHORIZED)))
                || result.as_ref().ok().is_some_and(|v| {
                    matches!(
                        v.pointer("/code").unwrap_or(&Value::Null).as_i64(),
                        Some(99991663 | 99991668)
                    )
                });
            if expired && !refreshed {
                self.tokens.invalidate().await;
                refreshed = true;
                continue;
            }
            let value = result?;
            check(&value)?;
            return Ok(value);
        }
    }

    fn address(&self, channel: &ChannelRef) -> Result<(), Error> {
        if channel.platform != "feishu"
            || channel.scope_id.as_deref() != Some(self.config.app_id.as_str())
        {
            return Err(Error::Invalid(
                "message does not belong to this Feishu application",
            ));
        }
        segment(&channel.channel_id)?;
        if let Some(thread) = &channel.thread_id {
            segment(thread)?;
        }
        Ok(())
    }

    async fn send_kind(
        &self,
        channel: &ChannelRef,
        kind: &str,
        content: Value,
    ) -> Result<MessageRef, Error> {
        self.address(channel)?;
        let (path, body) = if let Some(thread) = &channel.thread_id {
            (
                format!("/open-apis/im/v1/messages/{thread}/reply"),
                json!({"msg_type":kind,"content":content.to_string(),"reply_in_thread":true}),
            )
        } else {
            (
                "/open-apis/im/v1/messages?receive_id_type=chat_id".to_owned(),
                json!({"receive_id":channel.channel_id,"msg_type":kind,"content":content.to_string()}),
            )
        };
        let value = self.api(Method::POST, &path, Some(&body)).await?;
        let message_id = required(&value, "/data/message_id")?.to_owned();
        segment(&message_id)?;
        Ok(MessageRef {
            channel: channel.clone(),
            message_id,
        })
    }

    async fn send_inner(&self, channel: &ChannelRef, text: &str) -> Result<MessageRef, Error> {
        self.address(channel)?;
        if text.chars().count() > self.message_limit() {
            return Err(Error::TooLarge);
        }
        match self.config.delivery {
            Delivery::Text => self.send_kind(channel, "text", json!({"text":text})).await,
            Delivery::Post => self.post_fallback(channel, text, true).await,
            Delivery::Card => {
                let mut cards = self.cards.lock().await;
                if cards.len() >= 1024 {
                    let finished = cards.iter().find_map(|(id, state)| {
                        state
                            .try_lock()
                            .ok()
                            .filter(|state| state.finalized)
                            .map(|_| id.clone())
                    });
                    if let Some(id) = finished {
                        cards.remove(&id);
                    } else {
                        return Err(Error::Invalid("too many unfinished Feishu cards"));
                    }
                }
                let create = json!({"type":"card_json","data":card(text, true).to_string()});
                let value = match self
                    .api(Method::POST, "/open-apis/cardkit/v1/cards", Some(&create))
                    .await
                {
                    Ok(value) => value,
                    Err(error) if rich_rejected(&error) => {
                        return self.post_fallback(channel, text, true).await;
                    }
                    Err(error) => return Err(error),
                };
                let card_id = required(&value, "/data/card_id")?.to_owned();
                segment(&card_id)?;
                let message = match self
                    .send_kind(
                        channel,
                        "interactive",
                        json!({"type":"card","data":{"card_id":card_id}}),
                    )
                    .await
                {
                    Ok(message) => message,
                    Err(error) if rich_rejected(&error) => {
                        return self.post_fallback(channel, text, true).await;
                    }
                    Err(error) => return Err(error),
                };
                cards.insert(
                    message.message_id.clone(),
                    Arc::new(Mutex::new(CardState {
                        card_id,
                        sequence: 0,
                        finalized: false,
                    })),
                );
                Ok(message)
            }
        }
    }

    async fn post_fallback(
        &self,
        channel: &ChannelRef,
        text: &str,
        remember: bool,
    ) -> Result<MessageRef, Error> {
        let mut edits = if remember {
            Some(self.edits.lock().await)
        } else {
            None
        };
        if edits.as_ref().is_some_and(|edits| edits.len() >= 1024) {
            return Err(Error::Invalid("too many Feishu edit counters"));
        }
        let (message, kind) = match self.send_kind(channel, "post", post(text)).await {
            Ok(message) => (message, Delivery::Post),
            Err(error) if rich_rejected(&error) => (
                self.send_kind(channel, "text", json!({"text":text}))
                    .await?,
                Delivery::Text,
            ),
            Err(error) => return Err(error),
        };
        if let Some(edits) = edits.as_mut() {
            edits.insert(message.message_id.clone(), (kind, 0));
        }
        Ok(message)
    }

    async fn edit_inner(
        &self,
        message: &MessageRef,
        text: &str,
        final_update: bool,
    ) -> Result<(), Error> {
        self.address(&message.channel)?;
        segment(&message.message_id)?;
        if text.chars().count() > self.message_limit() {
            return Err(Error::TooLarge);
        }
        let state = self.cards.lock().await.get(&message.message_id).cloned();
        if let Some(state) = state {
            let mut state = state.lock().await;
            state.sequence = state
                .sequence
                .checked_add(1)
                .ok_or(Error::Invalid("card sequence exhausted"))?;
            let (path, body) = if final_update || state.finalized {
                (
                    format!("/open-apis/cardkit/v1/cards/{}", state.card_id),
                    json!({"card":{"type":"card_json","data":card(text,false).to_string()},"sequence":state.sequence}),
                )
            } else {
                (
                    format!(
                        "/open-apis/cardkit/v1/cards/{}/elements/md_stream/content",
                        state.card_id
                    ),
                    json!({"content":text,"sequence":state.sequence}),
                )
            };
            self.api(Method::PUT, &path, Some(&body)).await?;
            state.finalized |= final_update;
            return Ok(());
        }
        let mut edits = self.edits.lock().await;
        if self.config.delivery == Delivery::Card && !edits.contains_key(&message.message_id) {
            return Err(Error::Invalid("Feishu card state is unavailable"));
        }
        if edits.len() >= 1024 && !edits.contains_key(&message.message_id) {
            return Err(Error::Invalid("too many Feishu edit counters"));
        }
        let (kind, count) = edits
            .entry(message.message_id.clone())
            .or_insert((self.config.delivery, 0));
        if *count >= if final_update { 20 } else { 18 } {
            return Err(Error::Platform {
                code: "230072".into(),
                message: "Feishu message edit limit reached".into(),
            });
        }
        let content = match kind {
            Delivery::Text => json!({"text":text}),
            _ => post(text),
        };
        self.api(
            Method::PATCH,
            &format!("/open-apis/im/v1/messages/{}", message.message_id),
            Some(&json!({"content":content.to_string()})),
        )
        .await?;
        *count += 1;
        Ok(())
    }

    /// Replace the full CardKit card with a static card containing final text.
    /// This re-renders Markdown tables and disables the streaming cursor.
    pub async fn finalize(&self, message: &MessageRef, text: &str) -> Result<(), Error> {
        self.edit_inner(message, text, true).await
    }

    async fn reaction(&self, message: &MessageRef, emoji: &str, remove: bool) -> Result<(), Error> {
        self.address(&message.channel)?;
        segment(&message.message_id)?;
        let emoji_type = reaction_type(emoji).ok_or(Error::Invalid("unsupported Feishu emoji"))?;
        let path = format!("/open-apis/im/v1/messages/{}/reactions", message.message_id);
        if !remove {
            self.api(
                Method::POST,
                &path,
                Some(&json!({"reaction_type":{"emoji_type":emoji_type}})),
            )
            .await?;
            return Ok(());
        }
        let mut page = String::new();
        let mut pages = std::collections::HashSet::new();
        loop {
            let value = self
                .api(
                    Method::GET,
                    &format!("{path}?reaction_type={emoji_type}{page}"),
                    None,
                )
                .await?;
            if let Some(items) = value.pointer("/data/items").and_then(Value::as_array) {
                for item in items {
                    if item
                        .pointer("/operator/operator_id/open_id")
                        .and_then(Value::as_str)
                        == Some(self.bot_id.as_str())
                    {
                        let id = required(item, "/reaction_id")?;
                        segment(id)?;
                        self.api(Method::DELETE, &format!("{path}/{id}"), None)
                            .await?;
                    }
                }
            }
            if value.pointer("/data/has_more").and_then(Value::as_bool) != Some(true) {
                break;
            }
            let token = required(&value, "/data/page_token")?;
            if token.len() > 4096 || pages.len() >= 100 || !pages.insert(token.to_owned()) {
                return Err(Error::Invalid("invalid Feishu reaction pagination"));
            }
            let mut encoded = rig_reqwest::reqwest::Url::parse("https://localhost/")
                .map_err(|_| Error::Invalid("Feishu query encoding failed"))?;
            encoded.query_pairs_mut().append_pair("page_token", token);
            page = format!("&{}", encoded.query().unwrap_or_default());
        }
        Ok(())
    }
}

impl ChatAdapter for Feishu {
    fn platform(&self) -> &'static str {
        "feishu"
    }
    fn message_limit(&self) -> usize {
        4000
    }
    fn renders_native_tables(&self) -> bool {
        self.config.delivery == Delivery::Card
    }
    fn send<'a>(
        &'a self,
        channel: &'a ChannelRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>> {
        Box::pin(async move { self.send_inner(channel, text).await.map_err(Into::into) })
    }
    fn edit<'a>(
        &'a self,
        message: &'a MessageRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.edit_inner(message, text, false)
                .await
                .map_err(Into::into)
        })
    }
    fn edit_final<'a>(
        &'a self,
        message: &'a MessageRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move { self.finalize(message, text).await.map_err(Into::into) })
    }
    fn send_final<'a>(
        &'a self,
        channel: &'a ChannelRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            if text.chars().count() > self.message_limit() {
                return Err(Error::TooLarge.into());
            }
            if self.config.delivery == Delivery::Card {
                match self
                    .send_kind(channel, "interactive", card(text, false))
                    .await
                {
                    Ok(_) => {}
                    Err(error) if rich_rejected(&error) => {
                        self.post_fallback(channel, text, false).await?;
                    }
                    Err(error) => return Err(error.into()),
                }
            } else {
                self.send_inner(channel, text).await?;
            }
            Ok(())
        })
    }
    fn delete<'a>(&'a self, message: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.address(&message.channel)?;
            segment(&message.message_id)?;
            self.api(
                Method::DELETE,
                &format!("/open-apis/im/v1/messages/{}", message.message_id),
                None,
            )
            .await?;
            self.cards.lock().await.remove(&message.message_id);
            self.edits.lock().await.remove(&message.message_id);
            Ok(())
        })
    }
    fn add_reaction<'a>(
        &'a self,
        message: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.reaction(message, emoji, false)
                .await
                .map_err(Into::into)
        })
    }
    fn remove_reaction<'a>(
        &'a self,
        message: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.reaction(message, emoji, true)
                .await
                .map_err(Into::into)
        })
    }
}

impl Platform for Feishu {
    fn bot_id(&self) -> &str {
        &self.bot_id
    }
    fn receive<'a>(
        &'a self,
        request: WebhookRequest,
    ) -> WasmBoxedFuture<'a, Result<WebhookResponse, Error>> {
        Box::pin(self.webhook(request))
    }
    fn prepare<'a>(&'a self, incoming: Incoming) -> WasmBoxedFuture<'a, Result<Inbound, Error>> {
        Box::pin(self.prepare_media(incoming))
    }
}

fn required<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, Error> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(Error::Invalid("missing Feishu response field"))
}

fn segment(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.len() > 512
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err(Error::Invalid("invalid Feishu identifier"));
    }
    Ok(())
}

fn check(value: &Value) -> Result<(), Error> {
    match value.pointer("/code").unwrap_or(&Value::Null).as_i64() {
        Some(0) => Ok(()),
        Some(code) => Err(Error::Platform {
            code: code.to_string(),
            message: "Feishu API rejected the operation".into(),
        }),
        None => Err(Error::Invalid("missing Feishu API status")),
    }
}

fn rich_rejected(error: &Error) -> bool {
    match error {
        Error::Status(status) => matches!(
            *status,
            http::StatusCode::BAD_REQUEST
                | http::StatusCode::PAYLOAD_TOO_LARGE
                | http::StatusCode::UNSUPPORTED_MEDIA_TYPE
                | http::StatusCode::UNPROCESSABLE_ENTITY
        ),
        Error::Platform { code, .. } => {
            !code.starts_with("999916") && !matches!(code.as_str(), "230020" | "429")
        }
        _ => false,
    }
}

fn card(text: &str, streaming: bool) -> Value {
    let config = if streaming {
        json!({"streaming_mode":true,"streaming_config":{"print_strategy":"fast"}})
    } else {
        json!({"streaming_mode":false})
    };
    json!({"schema":"2.0","config":config,"body":{"elements":[{"tag":"markdown","element_id":"md_stream","content":text}]}})
}

fn post(text: &str) -> Value {
    json!({"zh_cn":{"title":"","content":[[{"tag":"md","text":text}]]}})
}

fn reaction_type(emoji: &str) -> Option<&'static str> {
    match emoji {
        "👀" => Some("EYES"),
        "🤔" => Some("THINKING"),
        "🔥" => Some("FIRE"),
        "👨‍💻" => Some("TECHNOLOGIST"),
        "⚡" => Some("LIGHTNING"),
        "🆗" => Some("OK"),
        "👍" => Some("THUMBSUP"),
        "😱" => Some("SCREAM"),
        "🥱" => Some("YAWN"),
        "😨" => Some("FEARFUL"),
        "❤️" => Some("HEART"),
        "🎉" => Some("PARTY"),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
