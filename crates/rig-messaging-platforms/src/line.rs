//! LINE Messaging API webhooks and text delivery.
//!
//! Signed callbacks are normalized without media downloads. Reply tokens are
//! consumed once; expired tokens use push delivery subject to account quotas.

//!
//! ```no_run
//! use rig_messaging_platforms::{Error, Http, line::{Line, LineConfig}};
//! fn connect(config: LineConfig) -> Result<Line, Error> {
//!     Line::new(config, Http::new(std::time::Duration::from_secs(60), 10 * 1024 * 1024)?)
//! }
//! ```

use crate::{Error, Http, Incoming, Platform, WebhookRequest, WebhookResponse, auth};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http::{Method, StatusCode};
use rig_core::wasm_compat::WasmBoxedFuture;
use rig_messaging::{
    Attachment, AttachmentSource, ChannelRef, ChatAdapter, ChatError, Inbound, MessageRef, Sender,
};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

/// LINE credentials and API endpoints. Never log this configuration.
pub struct LineConfig {
    pub channel_secret: String,
    pub channel_access_token: String,
    pub bot_id: String,
    pub api_base: String,
    pub data_base: String,
    pub media_limit: usize,
}

tokio::task_local! { static REPLY_TOKEN: Arc<Mutex<Option<ReplyToken>>>; }

struct ReplyToken {
    token: String,
    received: Instant,
    channel_key: String,
}

/// LINE authenticated ingress and outbound Messaging API adapter.
pub struct Line {
    config: LineConfig,
    http: Http,
}

impl Line {
    /// Construct an adapter with mandatory callback signature credentials.
    pub fn new(config: LineConfig, http: Http) -> Result<Self, Error> {
        if config.channel_secret.is_empty()
            || config.channel_access_token.is_empty()
            || config.bot_id.is_empty()
            || config.media_limit == 0
        {
            return Err(Error::Invalid("LINE credentials, identity and media limit"));
        }
        Ok(Self { config, http })
    }

    fn channel(&self, source: &Value) -> Result<ChannelRef, Error> {
        let key = match source.pointer("/type").unwrap_or(&Value::Null).as_str() {
            Some("user") => "userId",
            Some("group") => "groupId",
            Some("room") => "roomId",
            _ => return Err(Error::Invalid("LINE source type")),
        };
        Ok(ChannelRef {
            platform: "line".into(),
            scope_id: Some(self.config.bot_id.clone()),
            channel_id: field(source, key)?.into(),
            thread_id: None,
        })
    }

    fn normalize(&self, event: &Value) -> Result<Option<Incoming>, Error> {
        if event.pointer("/type").unwrap_or(&Value::Null).as_str() != Some("message") {
            return Ok(None);
        }
        let message = event.pointer("/message").unwrap_or(&Value::Null);
        if !matches!(
            message.pointer("/type").unwrap_or(&Value::Null).as_str(),
            Some("text" | "image" | "audio")
        ) {
            return Ok(None);
        }
        // Sender-less group events cannot satisfy a human allowlist safely.
        let Some(user_id) = event
            .pointer("/source/userId")
            .unwrap_or(&Value::Null)
            .as_str()
            .filter(|id| !id.is_empty())
        else {
            return Ok(None);
        };
        let channel = self.channel(event.pointer("/source").unwrap_or(&Value::Null))?;
        let text = message
            .pointer("/text")
            .unwrap_or(&Value::Null)
            .as_str()
            .unwrap_or("")
            .to_owned();
        let mentions_bot = message
            .pointer("/mention/mentionees")
            .unwrap_or(&Value::Null)
            .as_array()
            .into_iter()
            .flatten()
            .any(|mention| {
                mention.pointer("/isSelf").unwrap_or(&Value::Null).as_bool() == Some(true)
            });
        Ok(Some(Incoming {
            inbound: Inbound {
                message: MessageRef {
                    channel: channel.clone(),
                    message_id: field(message, "id")?.into(),
                },
                reply_channel: channel,
                sender: Sender {
                    id: user_id.into(),
                    name: user_id.into(),
                    is_bot: false,
                },
                text,
                attachments: Vec::new(),
                is_dm: event
                    .pointer("/source/type")
                    .unwrap_or(&Value::Null)
                    .as_str()
                    == Some("user"),
                is_thread: false,
                mentions_bot,
            },
            payload: event.clone(),
        }))
    }

    async fn api(&self, route: &str, body: &Value) -> Result<Value, Error> {
        let url = format!(
            "{}/v2/bot/message/{route}",
            self.config.api_base.trim_end_matches('/')
        );
        let (status, _, bytes) = self
            .http
            .response(
                self.http
                    .request(Method::POST, &url)
                    .bearer_auth(&self.config.channel_access_token)
                    .json(body),
            )
            .await?;
        if !status.is_success() {
            let invalid_token = status == StatusCode::BAD_REQUEST
                && route == "reply"
                && serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .and_then(|value| {
                        value
                            .pointer("/message")
                            .unwrap_or(&Value::Null)
                            .as_str()
                            .map(str::to_owned)
                    })
                    .is_some_and(|message| message.eq_ignore_ascii_case("Invalid reply token"));
            return if invalid_token {
                Err(Error::Platform {
                    code: "invalid_reply_token".into(),
                    message: "LINE reply token is unusable".into(),
                })
            } else {
                Err(Error::Status(status))
            };
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn validate_channel(&self, channel: &ChannelRef) -> Result<(), Error> {
        if channel.platform != "line"
            || channel.scope_id.as_deref() != Some(self.config.bot_id.as_str())
            || channel.channel_id.is_empty()
            || channel.thread_id.is_some()
        {
            return Err(Error::Invalid("LINE destination"));
        }
        Ok(())
    }

    /// Deliver text as five-message batches and return every real message id.
    /// This method splits at Unicode scalar boundaries; the router also formats its own chunks.
    pub async fn send_text(
        &self,
        channel: &ChannelRef,
        text: &str,
    ) -> Result<Vec<MessageRef>, Error> {
        self.validate_channel(channel)?;
        if text.is_empty() {
            return Err(Error::Invalid("LINE message length"));
        }
        let messages: Vec<Value> = text
            .chars()
            .collect::<Vec<_>>()
            .chunks(5000)
            .map(|chunk| json!({"type":"text", "text":chunk.iter().collect::<String>()}))
            .collect();
        let context = REPLY_TOKEN.try_with(Arc::clone).ok();
        let token = if let Some(context) = context {
            let mut token = context.lock().await;
            if token
                .as_ref()
                .is_some_and(|token| token.channel_key == channel.session_key())
            {
                token.take()
            } else {
                None
            }
        } else {
            None
        }
        .filter(|token| token.received.elapsed() < Duration::from_secs(55));
        let mut ids = Vec::new();
        for (index, batch) in messages.chunks(5).enumerate() {
            let result = if index == 0 {
                if let Some(token) = &token {
                    match self
                        .api(
                            "reply",
                            &json!({"replyToken":token.token, "messages":batch}),
                        )
                        .await
                    {
                        Ok(value) => value,
                        Err(Error::Platform { code, .. }) if code == "invalid_reply_token" => {
                            self.api("push", &json!({"to":channel.channel_id,"messages":batch}))
                                .await?
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    self.api("push", &json!({"to":channel.channel_id, "messages":batch}))
                        .await?
                }
            } else {
                self.api("push", &json!({"to":channel.channel_id, "messages":batch}))
                    .await?
            };
            let sent = result
                .pointer("/sentMessages")
                .unwrap_or(&Value::Null)
                .as_array()
                .ok_or(Error::Invalid("LINE sentMessages"))?;
            if sent.len() != batch.len() {
                return Err(Error::Invalid("LINE sent message count"));
            }
            for message in sent {
                ids.push(MessageRef {
                    channel: channel.clone(),
                    message_id: field(message, "id")?.into(),
                });
            }
        }
        Ok(ids)
    }

    async fn prepare_media(&self, mut incoming: Incoming) -> Result<Inbound, Error> {
        let event = &incoming.payload;
        let message = event.pointer("/message").unwrap_or(&Value::Null);
        if matches!(
            message.pointer("/type").unwrap_or(&Value::Null).as_str(),
            Some("image" | "audio")
        ) {
            if message
                .pointer("/contentProvider/type")
                .unwrap_or(&Value::Null)
                .as_str()
                == Some("external")
            {
                return Err(Error::Invalid("LINE external media is unsupported"));
            }
            let id = field(message, "id")?;
            if !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            {
                return Err(Error::Invalid("LINE media id"));
            }
            let url = format!(
                "{}/v2/bot/message/{id}/content",
                self.config.data_base.trim_end_matches('/')
            );
            let host = rig_reqwest::reqwest::Url::parse(&self.config.data_base)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .ok_or(Error::Invalid("LINE data endpoint"))?;
            let (headers, bytes) = self
                .http
                .download_response(
                    &url,
                    &[host.as_str()],
                    Some(&self.config.channel_access_token),
                    self.config.media_limit,
                )
                .await?;
            let image = message.pointer("/type").unwrap_or(&Value::Null).as_str() == Some("image");
            incoming.inbound.attachments.push(Attachment {
                filename: format!("line_{id}.{}", if image { "jpg" } else { "m4a" }),
                mime: headers
                    .get("content-type")
                    .and_then(|header| header.to_str().ok())
                    .and_then(|mime| mime.split(';').next())
                    .unwrap_or(if image { "image/jpeg" } else { "audio/mp4" })
                    .into(),
                size: Some(bytes.len() as u64),
                source: AttachmentSource::Bytes(bytes),
            });
        }
        Ok(incoming.inbound)
    }
}

fn received_millis() -> Result<u64, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .ok_or(Error::Invalid("LINE receipt time"))
}

fn field<'a>(value: &'a Value, key: &'static str) -> Result<&'a str, Error> {
    value[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(Error::Invalid(key))
}

impl Platform for Line {
    fn bot_id(&self) -> &str {
        &self.config.bot_id
    }
    fn receive<'a>(
        &'a self,
        request: WebhookRequest,
    ) -> WasmBoxedFuture<'a, Result<WebhookResponse, Error>> {
        Box::pin(async move {
            if request.method != Method::POST {
                return Err(Error::Authentication);
            }
            let signature = request
                .headers
                .get("x-line-signature")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| STANDARD.decode(value).ok())
                .ok_or(Error::Authentication)?;
            auth::verify_hmac_sha256(
                self.config.channel_secret.as_bytes(),
                &request.body,
                &signature,
            )?;
            let body: Value = serde_json::from_slice(&request.body)?;
            let mut events = Vec::new();
            for event in body
                .pointer("/events")
                .unwrap_or(&Value::Null)
                .as_array()
                .ok_or(Error::Invalid("LINE events"))?
            {
                if let Some(mut incoming) = self.normalize(event)? {
                    incoming
                        .payload
                        .as_object_mut()
                        .ok_or(Error::Invalid("LINE event object"))?
                        .insert("_rig_received_millis".into(), json!(received_millis()?));
                    events.push(incoming);
                }
            }
            Ok(WebhookResponse {
                status: StatusCode::OK,
                content_type: "application/json",
                body: Bytes::from_static(b"{}"),
                events,
            })
        })
    }
    fn prepare<'a>(&'a self, incoming: Incoming) -> WasmBoxedFuture<'a, Result<Inbound, Error>> {
        Box::pin(self.prepare_media(incoming))
    }
    fn scope<'a>(
        &'a self,
        event: Incoming,
        run: WasmBoxedFuture<'a, Result<(), Error>>,
    ) -> WasmBoxedFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let receipt = event
                .payload
                .pointer("/_rig_received_millis")
                .unwrap_or(&Value::Null)
                .as_u64();
            let age = receipt.and_then(|receipt| received_millis().ok()?.checked_sub(receipt));
            let token = event
                .payload
                .pointer("/replyToken")
                .unwrap_or(&Value::Null)
                .as_str()
                .filter(|token| !token.is_empty())
                .zip(age)
                .and_then(|(token, age)| {
                    if age >= 55_000 {
                        return None;
                    }
                    Some(ReplyToken {
                        token: token.into(),
                        received: Instant::now() - Duration::from_millis(age),
                        channel_key: event.inbound.reply_channel.session_key(),
                    })
                });
            REPLY_TOKEN.scope(Arc::new(Mutex::new(token)), run).await
        })
    }
}

impl ChatAdapter for Line {
    fn platform(&self) -> &'static str {
        "line"
    }
    fn message_limit(&self) -> usize {
        5000
    }
    fn supports_edit(&self) -> bool {
        false
    }
    fn supports_reactions(&self) -> bool {
        false
    }
    fn send<'a>(
        &'a self,
        channel: &'a ChannelRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>> {
        Box::pin(async move {
            if text.chars().count() > self.message_limit() {
                return Err(Error::Invalid("LINE message length").into());
            }
            self.send_text(channel, text)
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| Error::Invalid("LINE sent id").into())
        })
    }
    fn edit<'a>(
        &'a self,
        _message: &'a MessageRef,
        _text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("edit")) })
    }
    fn delete<'a>(
        &'a self,
        _message: &'a MessageRef,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("delete")) })
    }
    fn add_reaction<'a>(
        &'a self,
        _message: &'a MessageRef,
        _emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("add reaction")) })
    }
    fn remove_reaction<'a>(
        &'a self,
        _message: &'a MessageRef,
        _emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("remove reaction")) })
    }
}

#[cfg(test)]
mod tests;
