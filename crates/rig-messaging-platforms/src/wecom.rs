//! WeCom corporate application callbacks and direct user messages.
//!
//! Encrypted callbacks bind the signature, timestamp, and corporate identity.
//! [`WeCom::new`] validates credentials before constructing the adapter.
use crate::{Error, Http, Incoming, Platform, TokenCache, WebhookRequest, WebhookResponse, auth};
use ::http::Method;
use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::NoPadding};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use rig_core::wasm_compat::WasmBoxedFuture;
use rig_messaging::{
    Attachment, AttachmentSource, ChannelRef, ChatAdapter, ChatError, Inbound, MessageRef, Sender,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Credentials for a corporate application, separate from consumer WeChat.
pub struct Config {
    /// Corporate identifier used in callback plaintext and OAuth requests.
    pub corp_id: String,
    /// Numeric application identifier.
    pub agent_id: u64,
    /// Application secret for access tokens.
    pub secret: String,
    /// Callback signature token.
    pub callback_token: String,
    /// 43-character unpadded base64 AES key.
    pub encoding_aes_key: String,
}
/// Encrypted corporate callbacks and application REST delivery.
pub struct WeCom {
    config: Config,
    key: Vec<u8>,
    http: Http,
    tokens: TokenCache,
    base: String,
    bot_id: String,
}
impl WeCom {
    /// Validate credentials. Requests use finite deadlines and bounded bodies from `http`.
    pub fn new(config: Config, http: Http) -> Result<Self, Error> {
        if config.corp_id.is_empty()
            || config.agent_id == 0
            || config.secret.is_empty()
            || config.callback_token.is_empty()
            || config.encoding_aes_key.len() != 43
        {
            return Err(Error::Invalid("WeCom credentials"));
        }
        let key = STANDARD
            .decode(format!("{}=", config.encoding_aes_key))
            .map_err(|_| Error::Invalid("WeCom AES key"))?;
        if key.len() != 32 {
            return Err(Error::Invalid("WeCom AES key length"));
        }
        let bot_id = format!("{}:{}", config.corp_id, config.agent_id);
        Ok(Self {
            config,
            key,
            http,
            tokens: TokenCache::new(),
            base: "https://qyapi.weixin.qq.com".into(),
            bot_id,
        })
    }
    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<String, Error> {
        let mut url = rig_reqwest::reqwest::Url::parse(&format!("{}{path}", self.base))
            .map_err(|_| Error::Invalid("WeCom API URL"))?;
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        Ok(url.into())
    }
    async fn token(&self) -> Result<String, Error> {
        self.tokens
            .get_or_refresh(|| async {
                let url = self.url(
                    "/cgi-bin/gettoken",
                    &[
                        ("corpid", &self.config.corp_id),
                        ("corpsecret", &self.config.secret),
                    ],
                )?;
                let value: Value = self.http.json(self.http.request(Method::GET, &url)).await?;
                checked(&value)?;
                let token = required(&value, "access_token")?.to_owned();
                let ttl = value
                    .pointer("/expires_in")
                    .unwrap_or(&Value::Null)
                    .as_u64()
                    .ok_or(Error::Invalid("WeCom token expiry"))?;
                Ok((token, Duration::from_secs(ttl)))
            })
            .await
    }
    async fn api(&self, path: &str, body: &Value) -> Result<Value, Error> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let url = self.url(path, &[("access_token", &token)])?;
            let value: Value = self
                .http
                .json(self.http.request(Method::POST, &url).json(body))
                .await?;
            if expired(&value) && attempt == 0 {
                self.tokens.invalidate().await;
                continue;
            }
            checked(&value)?;
            return Ok(value);
        }
        Err(Error::Authentication)
    }
    fn channel(&self, channel: &ChannelRef) -> Result<(), Error> {
        if channel.platform != "wecom"
            || channel.scope_id.as_deref() != Some(self.config.corp_id.as_str())
            || channel.thread_id.is_some()
            || channel.channel_id.is_empty()
            || channel.channel_id.contains(['|', '@'])
        {
            return Err(Error::Invalid("WeCom direct user address"));
        }
        Ok(())
    }
    fn decrypt(&self, encrypted: &str) -> Result<String, Error> {
        let mut bytes = STANDARD
            .decode(encrypted)
            .map_err(|_| Error::Authentication)?;
        let iv = self.key.get(..16).ok_or(Error::Authentication)?;
        let cipher = cbc::Decryptor::<aes::Aes256>::new_from_slices(&self.key, iv)
            .map_err(|_| Error::Authentication)?;
        let plaintext = cipher
            .decrypt_padded::<NoPadding>(&mut bytes)
            .map_err(|_| Error::Authentication)?;
        let padding = usize::from(*plaintext.last().ok_or(Error::Authentication)?);
        if !(1..=32).contains(&padding)
            || padding > plaintext.len()
            || !plaintext
                .get(plaintext.len() - padding..)
                .ok_or(Error::Authentication)?
                .iter()
                .all(|byte| usize::from(*byte) == padding)
        {
            return Err(Error::Authentication);
        }
        let plaintext = plaintext
            .get(..plaintext.len() - padding)
            .ok_or(Error::Authentication)?;
        let length_bytes: [u8; 4] = plaintext
            .get(16..20)
            .ok_or(Error::Authentication)?
            .try_into()
            .map_err(|_| Error::Authentication)?;
        let end = 20usize
            .checked_add(u32::from_be_bytes(length_bytes) as usize)
            .ok_or(Error::Authentication)?;
        let message = plaintext.get(20..end).ok_or(Error::Authentication)?;
        auth::verify_secret(
            self.config.corp_id.as_bytes(),
            plaintext.get(end..).ok_or(Error::Authentication)?,
        )?;
        String::from_utf8(message.to_vec()).map_err(|_| Error::Authentication)
    }
    fn verified_payload(&self, request: &WebhookRequest, encrypted: &str) -> Result<String, Error> {
        let timestamp = request
            .query
            .get("timestamp")
            .ok_or(Error::Authentication)?;
        let nonce = request
            .query
            .get("nonce")
            .filter(|nonce| !nonce.is_empty())
            .ok_or(Error::Authentication)?;
        let supplied = request
            .query
            .get("msg_signature")
            .ok_or(Error::Authentication)?;
        let seconds = timestamp
            .parse::<u64>()
            .map_err(|_| Error::Authentication)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Authentication)?
            .as_secs();
        if now.abs_diff(seconds) > 300 {
            return Err(Error::Authentication);
        }
        let mut parts = [
            self.config.callback_token.as_str(),
            timestamp.as_str(),
            nonce.as_str(),
            encrypted,
        ];
        parts.sort_unstable();
        let signature = format!("{:x}", Sha1::digest(parts.concat().as_bytes()));
        auth::verify_secret(signature.as_bytes(), supplied.as_bytes())?;
        self.decrypt(encrypted)
    }
    async fn media(&self, media_id: &str, limit: usize) -> Result<Bytes, Error> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let url = self.url(
                "/cgi-bin/media/get",
                &[("access_token", &token), ("media_id", media_id)],
            )?;
            let bytes = self
                .http
                .execute_limited(self.http.request(Method::GET, &url), limit)
                .await?;
            if bytes.len() > limit {
                return Err(Error::TooLarge);
            }
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                if expired(&value) && attempt == 0 {
                    self.tokens.invalidate().await;
                    continue;
                }
                if value.get("errcode").is_some() {
                    checked(&value)?;
                    return Err(Error::Invalid("WeCom empty media"));
                }
            }
            return Ok(bytes);
        }
        Err(Error::Authentication)
    }
}
fn required<'a>(value: &'a Value, field: &str) -> Result<&'a str, Error> {
    value[field]
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or(Error::Invalid("WeCom response identity"))
}
fn expired(value: &Value) -> bool {
    matches!(
        value.pointer("/errcode").unwrap_or(&Value::Null).as_i64(),
        Some(40014 | 42001)
    )
}
fn checked(value: &Value) -> Result<(), Error> {
    match value.pointer("/errcode").unwrap_or(&Value::Null).as_i64() {
        Some(0) => Ok(()),
        Some(code) => Err(Error::Platform {
            code: code.to_string(),
            message: "WeCom request rejected".into(),
        }),
        None => Err(Error::Invalid("WeCom API result")),
    }
}
#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "Encrypt")]
    encrypted: String,
}
#[derive(Deserialize)]
struct Callback {
    #[serde(rename = "ToUserName")]
    corp: String,
    #[serde(rename = "FromUserName", default)]
    user: String,
    #[serde(rename = "MsgType")]
    kind: String,
    #[serde(rename = "MsgId", default)]
    id: String,
    #[serde(rename = "Content", default)]
    text: String,
    #[serde(rename = "MediaId", default)]
    media: String,
    #[serde(rename = "FileName", default)]
    filename: String,
    #[serde(rename = "AgentID", default)]
    agent: Option<u64>,
}
impl Platform for WeCom {
    fn bot_id(&self) -> &str {
        &self.bot_id
    }
    fn receive(
        &self,
        request: WebhookRequest,
    ) -> WasmBoxedFuture<'_, Result<WebhookResponse, Error>> {
        Box::pin(async move {
            if request.method == Method::GET {
                let encrypted = request.query.get("echostr").ok_or(Error::Authentication)?;
                let challenge = self.verified_payload(&request, encrypted)?;
                let mut response = WebhookResponse::ack();
                response.body = Bytes::from(challenge);
                return Ok(response);
            }
            if request.method != Method::POST {
                return Err(Error::Invalid("WeCom callback method"));
            }
            let envelope: Envelope = quick_xml::de::from_reader(request.body.as_ref())
                .map_err(|_| Error::Invalid("WeCom encrypted XML"))?;
            let xml = self.verified_payload(&request, &envelope.encrypted)?;
            let event: Callback =
                quick_xml::de::from_str(&xml).map_err(|_| Error::Invalid("WeCom callback XML"))?;
            if event.corp != self.config.corp_id
                || event
                    .agent
                    .is_some_and(|agent| agent != self.config.agent_id)
            {
                return Err(Error::Authentication);
            }
            let mut response = WebhookResponse::ack();
            if !matches!(event.kind.as_str(), "text" | "image" | "file") {
                return Ok(response);
            }
            if event.user.is_empty() || event.id.is_empty() {
                return Err(Error::Invalid("WeCom sender/message identity"));
            }
            let channel = ChannelRef {
                platform: "wecom".into(),
                scope_id: Some(self.config.corp_id.clone()),
                channel_id: event.user.clone(),
                thread_id: None,
            };
            let payload =
                json!({"kind": event.kind, "media": event.media, "filename": event.filename});
            response.events.push(Incoming {
                inbound: Inbound {
                    message: MessageRef {
                        channel: channel.clone(),
                        message_id: event.id,
                    },
                    reply_channel: channel,
                    sender: Sender {
                        id: event.user.clone(),
                        name: event.user,
                        is_bot: false,
                    },
                    text: event.text,
                    attachments: Vec::new(),
                    is_dm: true,
                    is_thread: false,
                    mentions_bot: false,
                },
                payload,
            });
            Ok(response)
        })
    }
    fn prepare(&self, mut event: Incoming) -> WasmBoxedFuture<'_, Result<Inbound, Error>> {
        Box::pin(async move {
            let kind = event
                .payload
                .pointer("/kind")
                .unwrap_or(&Value::Null)
                .as_str()
                .unwrap_or("text");
            if matches!(kind, "image" | "file") {
                let media_id = required(&event.payload, "media")?;
                let filename = event
                    .payload
                    .pointer("/filename")
                    .unwrap_or(&Value::Null)
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(if kind == "image" {
                        "image.jpg"
                    } else {
                        "attachment"
                    });
                let mime = if kind == "image" {
                    "image/jpeg"
                } else {
                    let extension = filename
                        .rsplit('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if ![
                        "txt", "csv", "log", "md", "json", "yaml", "yml", "toml", "xml", "rs",
                        "py", "js", "ts", "go", "html", "css", "sql", "sh",
                    ]
                    .contains(&extension.as_str())
                    {
                        return Err(Error::Invalid("WeCom file is not supported text"));
                    }
                    "text/plain"
                };
                let bytes = self
                    .media(
                        media_id,
                        if kind == "image" {
                            10 * 1024 * 1024
                        } else {
                            20 * 1024 * 1024
                        },
                    )
                    .await?;
                let mime = if kind == "image" {
                    image_mime(&bytes)?
                } else {
                    std::str::from_utf8(&bytes)
                        .map_err(|_| Error::Invalid("WeCom text file encoding"))?;
                    mime
                };
                event.inbound.attachments.push(Attachment {
                    filename: filename.into(),
                    mime: mime.into(),
                    size: Some(bytes.len() as u64),
                    source: AttachmentSource::Bytes(bytes),
                });
            }
            Ok(event.inbound)
        })
    }
}
fn image_mime(bytes: &[u8]) -> Result<&'static str, Error> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Ok("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Ok("image/gif");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Ok("image/webp");
    }
    Err(Error::Invalid("unsupported WeCom image format"))
}
impl ChatAdapter for WeCom {
    fn platform(&self) -> &'static str {
        "wecom"
    }
    fn message_limit(&self) -> usize {
        512
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
            self.channel(channel)?;
            if text.is_empty() || text.len() > 2048 {
                return Err(Error::Invalid("WeCom text byte limit").into());
            }
            let value = self.api("/cgi-bin/message/send", &json!({"touser":channel.channel_id,"msgtype":"text","agentid":self.config.agent_id,"text":{"content":text}})).await?;
            if value
                .pointer("/invaliduser")
                .unwrap_or(&Value::Null)
                .as_str()
                .is_some_and(|users| !users.is_empty())
            {
                return Err(Error::Invalid("WeCom recipient rejected").into());
            }
            Ok(MessageRef {
                channel: channel.clone(),
                message_id: required(&value, "msgid")?.into(),
            })
        })
    }
    fn edit<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("WeCom message edits")) })
    }
    fn delete<'a>(&'a self, message: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.channel(&message.channel)?;
            self.api(
                "/cgi-bin/message/recall",
                &json!({"msgid": message.message_id}),
            )
            .await?;
            Ok(())
        })
    }
    fn add_reaction<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("WeCom reactions")) })
    }
    fn remove_reaction<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("WeCom reactions")) })
    }
}
#[cfg(test)]
mod tests;
