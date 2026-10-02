//! Telegram Bot API ingress and messaging operations.
//!
//! Webhook verification precedes normalization. Media downloads run in `prepare`
//! after gateway admission. Private chat drafts are temporary previews.

//!
//! ```no_run
//! use rig_messaging_platforms::{Error, Http, telegram::{Telegram, TelegramConfig}};
//! fn connect(config: TelegramConfig) -> Result<Telegram, Error> {
//!     Telegram::new(config, Http::new(std::time::Duration::from_secs(60), 10 * 1024 * 1024)?)
//! }
//! ```

use crate::{Error, Http, Incoming, Platform, WebhookRequest, WebhookResponse};
use bytes::Bytes;
use chrono::DateTime;
use http::{Method, StatusCode};
use rig_core::wasm_compat::WasmBoxedFuture;
use rig_messaging::{
    Attachment, AttachmentSource, ChannelRef, ChatAdapter, ChatError, Inbound, MessageContext,
    MessageRef, Sender,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::Mutex;

/// Telegram credentials and transport endpoints. Never log this configuration.
pub struct TelegramConfig {
    pub bot_token: String,
    pub bot_id: String,
    pub bot_username: String,
    pub webhook_secret: String,
    pub api_base: String,
    pub media_limit: usize,
    pub rich_messages: bool,
}

/// A Telegram Bot API client and authenticated ingress adapter.
pub struct Telegram {
    config: TelegramConfig,
    http: Http,
    reactions: Mutex<HashMap<String, String>>,
}

impl Telegram {
    /// Construct an adapter with mandatory webhook authentication and bot identity.
    pub fn new(config: TelegramConfig, http: Http) -> Result<Self, Error> {
        if config.bot_token.is_empty()
            || config.bot_id.is_empty()
            || config.bot_username.is_empty()
            || config.webhook_secret.is_empty()
            || config.media_limit == 0
        {
            return Err(Error::Invalid(
                "Telegram credentials, identity and media limit",
            ));
        }
        Ok(Self {
            config,
            http,
            reactions: Mutex::new(HashMap::new()),
        })
    }

    async fn api(&self, method: &str, body: Value) -> Result<Value, Error> {
        let url = format!(
            "{}/bot{}/{method}",
            self.config.api_base.trim_end_matches('/'),
            self.config.bot_token
        );
        let (status, _, bytes) = self
            .http
            .response(self.http.request(Method::POST, &url).json(&body))
            .await?;
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) if !status.is_success() => return Err(Error::Status(status)),
            Err(error) => return Err(error.into()),
        };
        if method == "editMessageText"
            && value
                .pointer("/description")
                .unwrap_or(&Value::Null)
                .as_str()
                .is_some_and(|description| {
                    description.starts_with("Bad Request: message is not modified")
                })
        {
            return Ok(Value::Bool(true));
        }
        if !status.is_success()
            || value.pointer("/ok").unwrap_or(&Value::Null).as_bool() != Some(true)
        {
            let code = value
                .pointer("/error_code")
                .unwrap_or(&Value::Null)
                .as_u64()
                .unwrap_or(status.as_u16() as u64)
                .to_string();
            // Provider descriptions can echo URLs or credentials. Retain only the code.
            return Err(Error::Platform {
                code,
                message: "Telegram API rejected request".into(),
            });
        }
        Ok(value.pointer("/result").unwrap_or(&Value::Null).clone())
    }

    async fn send_plain_fallback(&self, mut body: Value) -> Result<Value, Error> {
        match self.api("sendMessage", body.clone()).await {
            Ok(value) => Ok(value),
            Err(Error::Platform { code, .. }) if code == "400" => {
                body.as_object_mut()
                    .ok_or(Error::Invalid("Telegram message body"))?
                    .remove("parse_mode");
                self.api("sendMessage", body).await
            }
            Err(error) => Err(error),
        }
    }

    /// Receive long-poll updates. Persist the next offset only after dispatching events.
    /// Disable the bot webhook before using this transport.
    pub async fn poll(
        &self,
        offset: i64,
        timeout_seconds: u32,
    ) -> Result<(i64, Vec<Incoming>), Error> {
        let updates = self.api("getUpdates", json!({"offset": offset, "timeout": timeout_seconds.min(50), "allowed_updates": ["message"]})).await?;
        let mut next = offset;
        let mut events = Vec::new();
        for update in updates
            .as_array()
            .ok_or(Error::Invalid("Telegram updates array"))?
        {
            let id = update
                .pointer("/update_id")
                .unwrap_or(&Value::Null)
                .as_i64()
                .ok_or(Error::Invalid("Telegram update id"))?;
            next = next.max(
                id.checked_add(1)
                    .ok_or(Error::Invalid("Telegram update id overflow"))?,
            );
            if let Some(event) = self.normalize(update)? {
                events.push(event);
            }
        }
        Ok((next, events))
    }

    /// Create a forum topic and return its address. The bot needs topic permissions.
    pub async fn create_topic(
        &self,
        channel: &ChannelRef,
        name: &str,
    ) -> Result<ChannelRef, Error> {
        self.validate_channel(channel)?;
        let result = self
            .api(
                "createForumTopic",
                json!({"chat_id": channel.channel_id, "name": name}),
            )
            .await?;
        let mut topic = channel.clone();
        topic.thread_id = Some(integer_id(&result, "message_thread_id")?);
        Ok(topic)
    }

    /// Stream an ephemeral rich draft in a known private chat. Send the final text separately.
    pub async fn send_draft(
        &self,
        channel: &ChannelRef,
        draft_id: i64,
        text: &str,
    ) -> Result<(), Error> {
        self.validate_channel(channel)?;
        // Telegram private chat ids are positive; groups and channels are negative.
        if channel.channel_id.parse::<i64>().map_or(true, |id| id <= 0)
            || draft_id == 0
            || text.chars().count() > 32768
        {
            return Err(Error::Invalid(
                "Telegram private draft address, id or text length",
            ));
        }
        self.api("sendRichMessageDraft", json!({"chat_id": channel.channel_id, "message_thread_id": topic_id(channel)?, "draft_id": draft_id, "rich_message": {"markdown": text}})).await?;
        Ok(())
    }

    /// Send up to 32768 characters using Telegram native rich formatting.
    /// Return the persistent platform id; transport errors do not trigger retries.
    pub async fn send_rich(&self, channel: &ChannelRef, text: &str) -> Result<MessageRef, Error> {
        self.validate_channel(channel)?;
        if text.is_empty() || text.chars().count() > 32768 {
            return Err(Error::Invalid("Telegram rich message length"));
        }
        let result = self.api("sendRichMessage",json!({"chat_id":channel.channel_id,"message_thread_id":topic_id(channel)?,"rich_message":{"markdown":text}})).await?;
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: integer_id(&result, "message_id")?,
        })
    }

    /// Send text quoting the original message with the same topic address.
    pub async fn reply(&self, message: &MessageRef, text: &str) -> Result<MessageRef, Error> {
        self.validate_channel(&message.channel)?;
        if text.is_empty() || text.chars().count() > 4096 {
            return Err(Error::Invalid("Telegram reply length"));
        }
        let result = self.send_plain_fallback(json!({"chat_id":message.channel.channel_id,"message_thread_id":topic_id(&message.channel)?,"reply_parameters":{"message_id":message_id(message)?},"text":text,"parse_mode":"Markdown"})).await?;
        Ok(MessageRef {
            channel: message.channel.clone(),
            message_id: integer_id(&result, "message_id")?,
        })
    }

    fn validate_channel(&self, channel: &ChannelRef) -> Result<(), Error> {
        if channel.platform != "telegram"
            || channel.scope_id.as_deref() != Some(self.config.bot_id.as_str())
            || channel.channel_id.parse::<i64>().is_err()
            || channel
                .thread_id
                .as_ref()
                .is_some_and(|id| id.parse::<i64>().is_err())
        {
            return Err(Error::Invalid("Telegram message address"));
        }
        Ok(())
    }

    fn normalize(&self, update: &Value) -> Result<Option<Incoming>, Error> {
        let Some(message) = update.get("message") else {
            return Ok(None);
        };
        let Some(from) = message.get("from") else {
            return Ok(None);
        };
        let sender_id = integer_id(from, "id")?;
        let chat = message.pointer("/chat").unwrap_or(&Value::Null);
        let channel = ChannelRef {
            platform: "telegram".into(),
            scope_id: Some(self.config.bot_id.clone()),
            channel_id: integer_id(chat, "id")?,
            thread_id: message
                .get("message_thread_id")
                .map(|_| integer_id(message, "message_thread_id"))
                .transpose()?,
        };
        let text = message
            .pointer("/text")
            .unwrap_or(&Value::Null)
            .as_str()
            .or_else(|| message.pointer("/caption").unwrap_or(&Value::Null).as_str())
            .unwrap_or("")
            .to_owned();
        let mentions_bot = message
            .pointer("/entities")
            .unwrap_or(&Value::Null)
            .as_array()
            .into_iter()
            .flatten()
            .chain(
                message
                    .pointer("/caption_entities")
                    .unwrap_or(&Value::Null)
                    .as_array()
                    .into_iter()
                    .flatten(),
            )
            .any(
                |entity| match entity.pointer("/type").unwrap_or(&Value::Null).as_str() {
                    Some("text_mention") => {
                        integer_id(entity.pointer("/user").unwrap_or(&Value::Null), "id")
                            .is_ok_and(|id| id == self.config.bot_id)
                    }
                    Some("mention") => entity
                        .pointer("/offset")
                        .unwrap_or(&Value::Null)
                        .as_u64()
                        .zip(entity.pointer("/length").unwrap_or(&Value::Null).as_u64())
                        .and_then(|(offset, length)| {
                            utf16_slice(&text, offset as usize, length as usize)
                        })
                        .is_some_and(|mention| {
                            mention
                                .trim_start_matches('@')
                                .eq_ignore_ascii_case(&self.config.bot_username)
                        }),
                    Some("bot_command") => entity
                        .pointer("/offset")
                        .unwrap_or(&Value::Null)
                        .as_u64()
                        .zip(entity.pointer("/length").unwrap_or(&Value::Null).as_u64())
                        .and_then(|(offset, length)| {
                            utf16_slice(&text, offset as usize, length as usize)
                        })
                        .and_then(|command| {
                            command.split_once('@').map(|(_, username)| {
                                username.eq_ignore_ascii_case(&self.config.bot_username)
                            })
                        })
                        .unwrap_or(false),
                    _ => false,
                },
            );
        let mentions: Vec<Sender> = message
            .pointer("/entities")
            .unwrap_or(&Value::Null)
            .as_array()
            .into_iter()
            .flatten()
            .chain(
                message
                    .pointer("/caption_entities")
                    .unwrap_or(&Value::Null)
                    .as_array()
                    .into_iter()
                    .flatten(),
            )
            .filter(|entity| {
                entity.pointer("/type").and_then(Value::as_str) == Some("text_mention")
            })
            .filter_map(|entity| {
                let user = entity.pointer("/user")?;
                let id = integer_id(user, "id").ok()?;
                let display_name = [
                    user.pointer("/first_name")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    user.pointer("/last_name")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                ]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
                let name = if display_name.is_empty() {
                    user.pointer("/username")
                        .and_then(Value::as_str)
                        .filter(|name| !name.is_empty())
                        .unwrap_or(&id)
                        .to_owned()
                } else {
                    display_name
                };
                Some(Sender {
                    id,
                    name,
                    is_bot: user.pointer("/is_bot").and_then(Value::as_bool) == Some(true),
                })
            })
            .collect();
        let has_media = ["photo", "document", "voice", "audio"]
            .iter()
            .any(|key| message.get(key).is_some());
        if text.trim().is_empty() && !has_media {
            return Ok(None);
        }
        let name = [
            from.pointer("/first_name")
                .unwrap_or(&Value::Null)
                .as_str()
                .unwrap_or(""),
            from.pointer("/last_name")
                .unwrap_or(&Value::Null)
                .as_str()
                .unwrap_or(""),
        ]
        .into_iter()
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
        let name = if name.is_empty() {
            from.pointer("/username")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(&sender_id)
                .to_owned()
        } else {
            name
        };
        let chat_name = chat
            .pointer("/title")
            .or_else(|| chat.pointer("/username"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                let first = chat.pointer("/first_name").and_then(Value::as_str)?;
                let last = chat
                    .pointer("/last_name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Some(if last.is_empty() {
                    first.to_owned()
                } else {
                    format!("{first} {last}")
                })
            });
        Ok(Some(Incoming {
            inbound: Inbound {
                message: MessageRef {
                    channel: channel.clone(),
                    message_id: integer_id(message, "message_id")?,
                },
                reply_channel: channel.clone(),
                context: MessageContext {
                    channel_name: chat_name,
                    sent_at: message
                        .pointer("/date")
                        .and_then(Value::as_i64)
                        .and_then(|seconds| DateTime::from_timestamp(seconds, 0)),
                    mentions,
                },
                sender: Sender {
                    id: sender_id,
                    name,
                    is_bot: from
                        .pointer("/is_bot")
                        .unwrap_or(&Value::Null)
                        .as_bool()
                        .unwrap_or(false),
                },
                text,
                attachments: Vec::new(),
                is_dm: message
                    .pointer("/chat/type")
                    .unwrap_or(&Value::Null)
                    .as_str()
                    == Some("private"),
                is_thread: channel.thread_id.is_some(),
                mentions_bot,
            },
            payload: message.clone(),
        }))
    }

    async fn prepare_media(&self, mut incoming: Incoming) -> Result<Inbound, Error> {
        let message = &incoming.payload;
        let media = message
            .pointer("/photo")
            .unwrap_or(&Value::Null)
            .as_array()
            .and_then(|photos| {
                photos.iter().max_by_key(|photo| {
                    photo
                        .pointer("/width")
                        .unwrap_or(&Value::Null)
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_mul(
                            photo
                                .pointer("/height")
                                .unwrap_or(&Value::Null)
                                .as_u64()
                                .unwrap_or(0),
                        )
                })
            })
            .map(|photo| (photo, "photo.jpg", "image/jpeg"))
            .or_else(|| {
                message
                    .get("document")
                    .map(|media| (media, "document", "application/octet-stream"))
            })
            .or_else(|| {
                message
                    .get("voice")
                    .map(|media| (media, "voice.ogg", "audio/ogg"))
            })
            .or_else(|| {
                message
                    .get("audio")
                    .map(|media| (media, "audio", "audio/mpeg"))
            });
        if let Some((media, fallback_name, fallback_mime)) = media {
            if media
                .pointer("/file_size")
                .unwrap_or(&Value::Null)
                .as_u64()
                .is_some_and(|size| size > self.config.media_limit as u64)
            {
                return Err(Error::TooLarge);
            }
            let file_id = media
                .pointer("/file_id")
                .unwrap_or(&Value::Null)
                .as_str()
                .ok_or(Error::Invalid("Telegram file id"))?;
            let file = self.api("getFile", json!({"file_id": file_id})).await?;
            if file
                .pointer("/file_size")
                .unwrap_or(&Value::Null)
                .as_u64()
                .is_some_and(|size| size > self.config.media_limit as u64)
            {
                return Err(Error::TooLarge);
            }
            let path = file
                .pointer("/file_path")
                .unwrap_or(&Value::Null)
                .as_str()
                .ok_or(Error::Invalid("Telegram file path"))?;
            if path.starts_with('/')
                || path.split('/').any(|part| part == "..")
                || path.contains(['?', '#', '\\'])
            {
                return Err(Error::Invalid("Telegram file path"));
            }
            let url = format!(
                "{}/file/bot{}/{path}",
                self.config.api_base.trim_end_matches('/'),
                self.config.bot_token
            );
            let host = reqwest_url_host(&self.config.api_base)?;
            let data = self
                .http
                .download(&url, &[host.as_str()], None, self.config.media_limit)
                .await?;
            incoming.inbound.attachments.push(Attachment {
                filename: media
                    .pointer("/file_name")
                    .unwrap_or(&Value::Null)
                    .as_str()
                    .unwrap_or(fallback_name)
                    .into(),
                mime: media
                    .pointer("/mime_type")
                    .unwrap_or(&Value::Null)
                    .as_str()
                    .unwrap_or(fallback_mime)
                    .into(),
                size: Some(data.len() as u64),
                source: AttachmentSource::Bytes(data),
            });
        }
        Ok(incoming.inbound)
    }
}

fn reqwest_url_host(url: &str) -> Result<String, Error> {
    rig_reqwest::reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .ok_or(Error::Invalid("Telegram API URL"))
}
fn message_id(message: &MessageRef) -> Result<i64, Error> {
    message
        .message_id
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or(Error::Invalid("Telegram message id"))
}
fn topic_id(channel: &ChannelRef) -> Result<Option<i64>, Error> {
    channel
        .thread_id
        .as_deref()
        .map(|id| {
            id.parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or(Error::Invalid("Telegram topic id"))
        })
        .transpose()
}
fn integer_id(value: &Value, key: &'static str) -> Result<String, Error> {
    value[key]
        .as_i64()
        .map(|id| id.to_string())
        .ok_or(Error::Invalid(key))
}
fn utf16_slice(text: &str, offset: usize, length: usize) -> Option<&str> {
    let end = offset.checked_add(length)?;
    let mut units = 0;
    let mut start_byte = None;
    let mut end_byte = None;
    for (byte, ch) in text.char_indices() {
        if units == offset {
            start_byte = Some(byte);
        }
        if units == end {
            end_byte = Some(byte);
            break;
        }
        units += ch.len_utf16();
    }
    if units == offset && start_byte.is_none() {
        start_byte = Some(text.len());
    }
    if units == end && end_byte.is_none() {
        end_byte = Some(text.len());
    }
    text.get(start_byte?..end_byte?)
}

impl Platform for Telegram {
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
            let secret = request
                .headers
                .get("x-telegram-bot-api-secret-token")
                .and_then(|header| header.to_str().ok())
                .ok_or(Error::Authentication)?;
            crate::auth::verify_secret(self.config.webhook_secret.as_bytes(), secret.as_bytes())?;
            let update: Value = serde_json::from_slice(&request.body)?;
            let events = self.normalize(&update)?.into_iter().collect();
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
}

impl ChatAdapter for Telegram {
    fn platform(&self) -> &'static str {
        "telegram"
    }
    fn message_limit(&self) -> usize {
        4096
    }
    fn send<'a>(
        &'a self,
        channel: &'a ChannelRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>> {
        Box::pin(async move {
            self.validate_channel(channel)?;
            if text.is_empty() || text.chars().count() > self.message_limit() {
                return Err(Error::Invalid("Telegram message length").into());
            }
            let body = json!({"chat_id": channel.channel_id, "message_thread_id": topic_id(channel)?, "text": text, "parse_mode":"Markdown"});
            let result = if self.config.rich_messages
                && (text.contains("| ") || text.contains("$$"))
            {
                match self.api("sendRichMessage", json!({"chat_id": channel.channel_id, "message_thread_id": topic_id(channel)?, "rich_message": {"markdown": text}})).await {
                    Ok(result) => result,
                    Err(Error::Platform { code, .. }) if code == "400" || code == "404" => self.send_plain_fallback(body).await?,
                    Err(error) => return Err(error.into()),
                }
            } else {
                self.send_plain_fallback(body).await?
            };
            Ok(MessageRef {
                channel: channel.clone(),
                message_id: integer_id(&result, "message_id")?,
            })
        })
    }
    fn edit<'a>(
        &'a self,
        message: &'a MessageRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.validate_channel(&message.channel)?;
            if text.is_empty() || text.chars().count() > self.message_limit() {
                return Err(Error::Invalid("Telegram edit length").into());
            }
            self.api("editMessageText", json!({"chat_id": message.channel.channel_id, "message_id": message_id(message)?, "text": text})).await?;
            Ok(())
        })
    }
    fn edit_final<'a>(
        &'a self,
        message: &'a MessageRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            if text.is_empty() || text.chars().count() > self.message_limit() {
                return Err(Error::Invalid("Telegram final edit length").into());
            }
            if self.config.rich_messages && (text.contains("| ") || text.contains("$$")) {
                self.validate_channel(&message.channel)?;
                match self.api("editMessageText", json!({"chat_id":message.channel.channel_id,"message_id":message_id(message)?,"rich_message":{"markdown":text}})).await {
                    Ok(_) => return Ok(()),
                    Err(Error::Platform {code, ..}) if code == "400" => {},
                    Err(error) => return Err(error.into()),
                }
            }
            self.edit(message, text).await
        })
    }
    fn delete<'a>(&'a self, message: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.validate_channel(&message.channel)?;
            self.api(
                "deleteMessage",
                json!({"chat_id": message.channel.channel_id, "message_id": message_id(message)?}),
            )
            .await?;
            Ok(())
        })
    }
    fn add_reaction<'a>(
        &'a self,
        message: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.validate_channel(&message.channel)?;
            let key = format!("{}:{}", message.channel.session_key(), message.message_id);
            let emoji = if emoji == "🆗" { "👍" } else { emoji };
            let mut reactions = self.reactions.lock().await;
            if reactions.len() >= 4096 && !reactions.contains_key(&key) {
                return Err(Error::Invalid("Telegram reaction cache capacity").into());
            }
            self.api("setMessageReaction", json!({"chat_id": message.channel.channel_id, "message_id": message_id(message)?, "reaction": [{"type": "emoji", "emoji": emoji}]})).await?;
            reactions.insert(key, emoji.into());
            Ok(())
        })
    }
    fn remove_reaction<'a>(
        &'a self,
        message: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.validate_channel(&message.channel)?;
            let key = format!("{}:{}", message.channel.session_key(), message.message_id);
            let emoji = if emoji == "🆗" { "👍" } else { emoji };
            let mut reactions = self.reactions.lock().await;
            if reactions.get(&key).is_some_and(|current| current != emoji) {
                return Ok(());
            }
            self.api("setMessageReaction", json!({"chat_id": message.channel.channel_id, "message_id": message_id(message)?, "reaction": []})).await?;
            reactions.remove(&key);
            Ok(())
        })
    }
    fn renders_native_tables(&self) -> bool {
        self.config.rich_messages
    }
}

#[cfg(test)]
mod tests;
