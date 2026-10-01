//! Signed LINE WORKS callbacks and service-account authenticated bot delivery.
//!
//! Configure a bot secret and RSA service-account key before registering the
//! webhook endpoint. Group messages require a boundary-delimited bot mention.
//!
//! ```
//! use rig_messaging::ChatAdapter;
//! use rig_messaging_platforms::lineworks::LineWorks;
//! fn can_edit(bot: &LineWorks) -> bool { bot.supports_edit() }
//! ```
use crate::{Error, Http, Incoming, Platform, TokenCache, WebhookRequest, WebhookResponse, auth};
use ::http::{Method, StatusCode};
use base64::{Engine, engine::general_purpose::STANDARD};
use rig_core::wasm_compat::WasmBoxedFuture;
use rig_messaging::{
    Attachment, AttachmentSource, ChannelRef, ChatAdapter, ChatError, Inbound, MessageRef, Sender,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Credentials and network policy for one LINE WORKS bot.
pub struct LineWorksConfig {
    /// Numeric bot identity.
    pub bot_id: String,
    /// Callback signing secret.
    pub bot_secret: String,
    /// Display name used by group mention matching.
    pub bot_name: String,
    /// OAuth client identity.
    pub client_id: String,
    /// OAuth client secret.
    pub client_secret: String,
    /// Service-account identity.
    pub service_account: String,
    /// RSA private key in PEM format.
    pub private_key: String,
    /// Bot API base URL.
    pub api_base: String,
    /// OAuth token endpoint.
    pub token_url: String,
    /// Maximum downloaded attachment bytes.
    pub media_limit: usize,
    /// Exact HTTPS hosts authorized to receive the media bearer token.
    pub media_hosts: Vec<String>,
    /// Send flexible text bubbles before plain-text fallback on validation failure.
    pub rich_messages: bool,
}
/// Bot transport with serialized OAuth token refresh.
pub struct LineWorks {
    config: LineWorksConfig,
    http: Http,
    tokens: TokenCache,
    key: jsonwebtoken::EncodingKey,
}
impl LineWorks {
    /// Validate mandatory credentials and the RSA key before serving callbacks.
    pub fn new(config: LineWorksConfig) -> Result<Self, Error> {
        if [
            &config.bot_id,
            &config.bot_secret,
            &config.bot_name,
            &config.client_id,
            &config.client_secret,
            &config.service_account,
        ]
        .iter()
        .any(|value| value.is_empty())
            || config.media_limit == 0
        {
            return Err(Error::Invalid("LINE WORKS credentials or media limit"));
        }
        if !config.bot_id.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::Invalid("LINE WORKS bot id"));
        }
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(config.private_key.as_bytes())
            .map_err(|_| Error::Invalid("LINE WORKS RSA key"))?;
        let http = Http::new(Duration::from_secs(30), config.media_limit.max(1024 * 1024))?;
        Ok(Self {
            config,
            http,
            tokens: TokenCache::new(),
            key,
        })
    }
    async fn token(&self) -> Result<String, Error> {
        self.tokens.get_or_refresh(|| async {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| Error::Invalid("system clock"))?.as_secs();
            let claims = json!({"iss":self.config.client_id,"sub":self.config.service_account,"iat":now,"exp":now+3600});
            let assertion = jsonwebtoken::encode(&jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256), &claims, &self.key).map_err(|_| Error::Authentication)?;
            let response: Value = self.http.json(self.http.request(Method::POST, &self.config.token_url).form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"), ("assertion", assertion.as_str()), ("client_id", self.config.client_id.as_str()), ("client_secret", self.config.client_secret.as_str()), ("scope", "bot.message,bot.read")
            ])).await?;
            let token = field(&response, "access_token")?.to_owned();
            let expiry = response.pointer("/expires_in").unwrap_or(&Value::Null).as_u64().or_else(|| response.pointer("/expires_in").unwrap_or(&Value::Null).as_str().and_then(|value| value.parse().ok())).ok_or(Error::Invalid("LINE WORKS token expiry"))?;
            Ok((token, Duration::from_secs(expiry)))
        }).await
    }
    fn endpoint(&self, channel: &ChannelRef) -> Result<String, Error> {
        if channel.platform != "lineworks" {
            return Err(Error::Invalid("LINE WORKS channel platform"));
        }
        let (kind, id) = match channel.channel_id.strip_prefix("user:") {
            Some(id) => ("users", id),
            None => ("channels", channel.channel_id.as_str()),
        };
        let mut url = rig_reqwest::reqwest::Url::parse(&self.config.api_base)
            .map_err(|_| Error::Invalid("LINE WORKS API URL"))?;
        url.path_segments_mut()
            .map_err(|_| Error::Invalid("LINE WORKS API URL"))?
            .pop_if_empty()
            .extend(["bots", &self.config.bot_id, kind, id, "messages"]);
        Ok(url.into())
    }
    async fn send_body(&self, url: &str, body: &Value) -> Result<(), Error> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let (status, _, _) = self
                .http
                .response(
                    self.http
                        .request(Method::POST, url)
                        .bearer_auth(token)
                        .json(body),
                )
                .await?;
            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.tokens.invalidate().await;
                continue;
            }
            if !status.is_success() {
                return Err(Error::Status(status));
            }
            return Ok(());
        }
        Err(Error::Authentication)
    }
    /// Deliver text or a flexible text bubble, preserving plain text on Flex validation failure.
    pub async fn send_text(&self, channel: &ChannelRef, text: &str) -> Result<(), Error> {
        if text.is_empty() || text.chars().count() > 10_000 {
            return Err(Error::Invalid("LINE WORKS text length"));
        }
        let url = self.endpoint(channel)?;
        if self.config.rich_messages && text.len() <= 12_000 {
            let body = json!({"content":{"type":"flex","altText":text.chars().take(400).collect::<String>(),"contents":{"type":"bubble","body":{"type":"box","layout":"vertical","contents":[{"type":"text","text":text,"wrap":true}]}}}});
            match self.send_body(&url, &body).await {
                Ok(()) => return Ok(()),
                Err(Error::Status(StatusCode::BAD_REQUEST)) => {}
                Err(error) => return Err(error),
            }
        }
        self.send_body(&url, &json!({"content":{"type":"text","text":text}}))
            .await
    }
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, Error> {
    value[name]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(Error::Invalid("LINE WORKS required field"))
}
fn mentioned(text: &str, name: &str) -> bool {
    let mention = format!("@{name}");
    text.match_indices(&mention).any(|(offset, _)| {
        text.get(..offset)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|c| !c.is_alphanumeric())
            && text
                .get(offset + mention.len()..)
                .and_then(|after| after.chars().next())
                .is_none_or(|c| !c.is_alphanumeric())
    })
}
impl Platform for LineWorks {
    fn bot_id(&self) -> &str {
        &self.config.bot_id
    }
    fn receive(
        &self,
        request: WebhookRequest,
    ) -> WasmBoxedFuture<'_, Result<WebhookResponse, Error>> {
        Box::pin(async move {
            if request.method != Method::POST {
                return Err(Error::Invalid("LINE WORKS callback method"));
            }
            let signature = request
                .headers
                .get("x-works-signature")
                .and_then(|value| value.to_str().ok())
                .ok_or(Error::Authentication)?;
            let signature = STANDARD
                .decode(signature)
                .map_err(|_| Error::Authentication)?;
            auth::verify_hmac_sha256(self.config.bot_secret.as_bytes(), &request.body, &signature)?;
            let bot = request
                .headers
                .get("x-works-botid")
                .and_then(|value| value.to_str().ok())
                .ok_or(Error::Authentication)?;
            auth::verify_secret(self.config.bot_id.as_bytes(), bot.as_bytes())?;
            let event: Value = serde_json::from_slice(&request.body)?;
            let mut response = WebhookResponse::ack();
            if event.pointer("/type").unwrap_or(&Value::Null) != "message" {
                return Ok(response);
            }
            let user = field(event.pointer("/source").unwrap_or(&Value::Null), "userId")?;
            let issued = field(&event, "issuedTime")?;
            let domain = event
                .pointer("/source/domainId")
                .unwrap_or(&Value::Null)
                .as_u64()
                .ok_or(Error::Invalid("LINE WORKS domain id"))?;
            let group = event
                .pointer("/source/channelId")
                .unwrap_or(&Value::Null)
                .as_str()
                .filter(|value| !value.is_empty());
            let channel = ChannelRef {
                platform: "lineworks".into(),
                scope_id: Some(domain.to_string()),
                channel_id: group
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("user:{user}")),
                thread_id: None,
            };
            let kind = field(event.pointer("/content").unwrap_or(&Value::Null), "type")?;
            if !matches!(kind, "text" | "image" | "file" | "audio" | "video") {
                return Ok(response);
            }
            let text = event
                .pointer("/content/text")
                .unwrap_or(&Value::Null)
                .as_str()
                .unwrap_or_default();
            let identity = format!(
                "{issued}:{}",
                STANDARD.encode(Sha256::digest(&request.body))
            );
            response.events.push(Incoming {
                inbound: Inbound {
                    message: MessageRef {
                        channel: channel.clone(),
                        message_id: identity,
                    },
                    reply_channel: channel,
                    sender: Sender {
                        id: user.into(),
                        name: user.into(),
                        is_bot: user == self.config.bot_id,
                    },
                    text: text.into(),
                    attachments: Vec::new(),
                    is_dm: group.is_none(),
                    is_thread: false,
                    mentions_bot: mentioned(text, &self.config.bot_name),
                },
                payload: event,
            });
            Ok(response)
        })
    }
    fn prepare(&self, mut event: Incoming) -> WasmBoxedFuture<'_, Result<Inbound, Error>> {
        Box::pin(async move {
            let content = event.payload.pointer("/content").unwrap_or(&Value::Null);
            if content.pointer("/type").unwrap_or(&Value::Null) == "text" {
                return Ok(event.inbound);
            }
            let file = field(content, "fileId")?;
            let mut url = rig_reqwest::reqwest::Url::parse(&self.config.api_base)
                .map_err(|_| Error::Invalid("LINE WORKS media URL"))?;
            url.path_segments_mut()
                .map_err(|_| Error::Invalid("LINE WORKS media URL"))?
                .pop_if_empty()
                .extend(["bots", &self.config.bot_id, "attachments", file]);
            let mut redirect = None;
            for attempt in 0..2 {
                let token = self.token().await?;
                let (status, headers, _) = self
                    .http
                    .response(
                        self.http
                            .request(Method::GET, url.as_str())
                            .bearer_auth(token),
                    )
                    .await?;
                if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                    self.tokens.invalidate().await;
                    continue;
                }
                if status != StatusCode::FOUND {
                    return Err(Error::Status(status));
                }
                redirect = Some(
                    headers
                        .get(::http::header::LOCATION)
                        .and_then(|value| value.to_str().ok())
                        .ok_or(Error::Invalid("LINE WORKS media redirect"))?
                        .to_owned(),
                );
                break;
            }
            let redirect = redirect.ok_or(Error::Authentication)?;
            let hosts: Vec<&str> = self.config.media_hosts.iter().map(String::as_str).collect();
            let (headers, body) = self
                .http
                .download_response(&redirect, &hosts, None, self.config.media_limit)
                .await?;
            let mime = headers
                .get(::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .unwrap_or("application/octet-stream")
                .to_owned();
            event.inbound.attachments.push(Attachment {
                filename: content
                    .pointer("/fileName")
                    .unwrap_or(&Value::Null)
                    .as_str()
                    .unwrap_or("attachment")
                    .into(),
                mime,
                size: Some(body.len() as u64),
                source: AttachmentSource::Bytes(body),
            });
            Ok(event.inbound)
        })
    }
}
impl ChatAdapter for LineWorks {
    fn platform(&self) -> &'static str {
        "lineworks"
    }
    fn message_limit(&self) -> usize {
        10_000
    }
    fn supports_edit(&self) -> bool {
        false
    }
    fn supports_reactions(&self) -> bool {
        false
    }
    fn send<'a>(
        &'a self,
        _: &'a ChannelRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>> {
        Box::pin(async {
            Err(ChatError::Unsupported(
                "LINE WORKS message address unavailable",
            ))
        })
    }
    fn send_final<'a>(
        &'a self,
        channel: &'a ChannelRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move { self.send_text(channel, text).await.map_err(ChatError::from) })
    }
    fn edit<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("LINE WORKS edit")) })
    }
    fn delete<'a>(&'a self, _: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("LINE WORKS delete")) })
    }
    fn add_reaction<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("LINE WORKS reactions")) })
    }
    fn remove_reaction<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("LINE WORKS reactions")) })
    }
}
#[cfg(test)]
mod tests;
