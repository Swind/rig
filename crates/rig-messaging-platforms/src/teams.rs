//! Microsoft Teams Bot Connector authentication, activities and message delivery.
//!
//! Signed service URLs and explicitly trusted media hosts constrain authenticated
//! HTTP calls. Attachment downloads happen after gateway admission.
//!
//! ```no_run
//! use rig_messaging_platforms::{Error, Http, teams::{Teams, TeamsConfig}};
//! fn connect(config: TeamsConfig) -> Result<Teams, Error> {
//!     Teams::new(config, Http::new(std::time::Duration::from_secs(30), 10 * 1024 * 1024)?)
//! }
//! ```

use crate::{
    Error, Http, Incoming, Platform, TokenCache, WebhookRequest, WebhookResponse, auth::JwtVerifier,
};
use http::Method;
use rig_core::wasm_compat::WasmBoxedFuture;
use rig_messaging::{
    Attachment, AttachmentSource, ChannelRef, ChatAdapter, ChatError, Inbound, MessageRef, Sender,
};
use rig_reqwest::reqwest::Url;
use serde_json::{Value, json};
use std::{collections::HashMap, time::Duration};
use tokio::sync::Mutex;

/// Bot registration, OAuth authority and trusted endpoint configuration.
pub struct TeamsConfig {
    pub app_id: String,
    pub app_secret: String,
    pub tenant_id: String,
    /// Empty derives the Microsoft tenant endpoint; otherwise an explicit trusted endpoint.
    pub oauth_endpoint: String,
    /// Pinned HTTPS key document, normally login.botframework.com/v1/.well-known/keys.
    pub jwks_url: String,
    pub allowed_tenants: Vec<String>,
    /// Exact HTTPS hosts accepted for signed Connector service URLs.
    pub service_hosts: Vec<String>,
    /// Exact HTTPS hosts accepted for file download URLs without Connector credentials.
    pub media_hosts: Vec<String>,
    pub media_limit: usize,
}

/// Teams Bot Connector adapter with cached OAuth and verified conversation references.
pub struct Teams {
    config: TeamsConfig,
    http: Http,
    verifier: JwtVerifier,
    tokens: TokenCache,
    conversations: Mutex<HashMap<String, String>>,
    #[cfg(test)]
    connector_base: Option<Url>,
}

impl Teams {
    /// Construct a bot with required JWT and endpoint trust policies.
    pub fn new(mut config: TeamsConfig, http: Http) -> Result<Self, Error> {
        if config.app_id.is_empty()
            || config.app_secret.is_empty()
            || config.tenant_id.is_empty()
            || config.media_limit == 0
            || config.service_hosts.is_empty()
            || !config
                .tenant_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
        {
            return Err(Error::Invalid(
                "Teams credentials, tenant and endpoint policy",
            ));
        }
        if config.oauth_endpoint.is_empty() {
            config.oauth_endpoint = format!(
                "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
                config.tenant_id
            );
        }
        let verifier = JwtVerifier::new(
            http.clone(),
            config.jwks_url.clone(),
            "https://api.botframework.com",
            &config.app_id,
        )?;
        Ok(Self {
            config,
            http,
            verifier,
            tokens: TokenCache::new(),
            conversations: Mutex::new(HashMap::new()),
            #[cfg(test)]
            connector_base: None,
        })
    }

    async fn token(&self) -> Result<String, Error> {
        self.tokens
            .get_or_refresh(|| async {
                let value: Value = self
                    .http
                    .json(
                        self.http
                            .request(Method::POST, &self.config.oauth_endpoint)
                            .form(&[
                                ("grant_type", "client_credentials"),
                                ("client_id", self.config.app_id.as_str()),
                                ("client_secret", self.config.app_secret.as_str()),
                                ("scope", "https://api.botframework.com/.default"),
                            ]),
                    )
                    .await?;
                let token = field(&value, "access_token")?.to_owned();
                let lifetime = value
                    .pointer("/expires_in")
                    .unwrap_or(&Value::Null)
                    .as_u64()
                    .ok_or(Error::Invalid("Teams token expiry"))?;
                Ok((token, Duration::from_secs(lifetime)))
            })
            .await
    }

    fn service_url(&self, service: &str) -> Result<Url, Error> {
        let url = trusted_url(service, &self.config.service_hosts)?;
        if url.query().is_some() {
            return Err(Error::Invalid("Teams service URL query"));
        }
        Ok(url)
    }

    fn normalize(&self, activity: &Value) -> Result<Option<Incoming>, Error> {
        if activity.pointer("/type").unwrap_or(&Value::Null).as_str() != Some("message") {
            return Ok(None);
        }
        let tenant = activity
            .pointer("/channelData/tenant/id")
            .unwrap_or(&Value::Null)
            .as_str()
            .or_else(|| {
                activity
                    .pointer("/conversation/tenantId")
                    .unwrap_or(&Value::Null)
                    .as_str()
            })
            .or_else(|| {
                activity
                    .pointer("/tenant/id")
                    .unwrap_or(&Value::Null)
                    .as_str()
            })
            .filter(|id| !id.is_empty())
            .ok_or(Error::Invalid("Teams tenant identity"))?;
        if !self.config.allowed_tenants.is_empty()
            && !self
                .config
                .allowed_tenants
                .iter()
                .any(|allowed| allowed == tenant)
        {
            return Err(Error::Authentication);
        }
        let id = field(activity, "id")?;
        let sender_id = field(activity.pointer("/from").unwrap_or(&Value::Null), "id")?;
        let conversation = activity.pointer("/conversation").unwrap_or(&Value::Null);
        let kind = conversation
            .pointer("/conversationType")
            .unwrap_or(&Value::Null)
            .as_str()
            .unwrap_or("personal");
        let thread_id = if kind == "channel" {
            Some(
                activity
                    .pointer("/replyToId")
                    .unwrap_or(&Value::Null)
                    .as_str()
                    .unwrap_or(id)
                    .into(),
            )
        } else {
            None
        };
        let channel = ChannelRef {
            platform: "teams".into(),
            scope_id: Some(tenant.into()),
            channel_id: field(conversation, "id")?.into(),
            thread_id,
        };
        let mut text = activity
            .pointer("/text")
            .unwrap_or(&Value::Null)
            .as_str()
            .unwrap_or("")
            .to_owned();
        let mut mentions_bot = false;
        for entity in activity
            .pointer("/entities")
            .unwrap_or(&Value::Null)
            .as_array()
            .into_iter()
            .flatten()
        {
            if entity.pointer("/type").unwrap_or(&Value::Null).as_str() == Some("mention")
                && entity
                    .pointer("/mentioned/id")
                    .unwrap_or(&Value::Null)
                    .as_str()
                    .is_some_and(|id| self.own_id(id))
            {
                mentions_bot = true;
                if let Some(mention) = entity.pointer("/text").unwrap_or(&Value::Null).as_str() {
                    text = text.replace(mention, "");
                }
            }
        }
        if text.trim().is_empty()
            && activity
                .pointer("/attachments")
                .unwrap_or(&Value::Null)
                .as_array()
                .is_none_or(Vec::is_empty)
        {
            return Ok(None);
        }
        Ok(Some(Incoming {
            inbound: Inbound {
                message: MessageRef {
                    channel: channel.clone(),
                    message_id: id.into(),
                },
                reply_channel: channel.clone(),
                sender: Sender {
                    id: if self.own_id(sender_id) {
                        self.config.app_id.clone()
                    } else {
                        sender_id.into()
                    },
                    name: activity
                        .pointer("/from/name")
                        .unwrap_or(&Value::Null)
                        .as_str()
                        .unwrap_or(sender_id)
                        .into(),
                    is_bot: self.own_id(sender_id)
                        || activity
                            .pointer("/from/role")
                            .unwrap_or(&Value::Null)
                            .as_str()
                            == Some("bot"),
                },
                text: text.trim().into(),
                attachments: Vec::new(),
                is_dm: kind == "personal",
                is_thread: kind == "channel"
                    && activity
                        .pointer("/replyToId")
                        .unwrap_or(&Value::Null)
                        .as_str()
                        .is_some(),
                mentions_bot,
            },
            payload: activity.clone(),
        }))
    }

    fn own_id(&self, id: &str) -> bool {
        id.strip_prefix("28:").unwrap_or(id) == self.config.app_id
    }

    async fn prepare_event(&self, mut event: Incoming) -> Result<Inbound, Error> {
        let service = field(&event.payload, "serviceUrl")?;
        let service_url = self.service_url(service)?;
        let mut remaining = self.config.media_limit;
        for attachment in event
            .payload
            .pointer("/attachments")
            .unwrap_or(&Value::Null)
            .as_array()
            .into_iter()
            .flatten()
        {
            let mime = field(attachment, "contentType")?;
            let is_file = mime == "application/vnd.microsoft.teams.file.download.info";
            let (url, mime, name, authenticated, hosts) = if is_file {
                (
                    field(
                        attachment.pointer("/content").unwrap_or(&Value::Null),
                        "downloadUrl",
                    )?
                    .to_owned(),
                    attachment
                        .pointer("/content/fileType")
                        .unwrap_or(&Value::Null)
                        .as_str()
                        .map(file_mime)
                        .unwrap_or("application/octet-stream")
                        .to_owned(),
                    attachment
                        .pointer("/name")
                        .unwrap_or(&Value::Null)
                        .as_str()
                        .unwrap_or("attachment")
                        .to_owned(),
                    false,
                    self.config.media_hosts.clone(),
                )
            } else if let Some(url) = attachment
                .pointer("/contentUrl")
                .unwrap_or(&Value::Null)
                .as_str()
            {
                let host = service_url
                    .host_str()
                    .ok_or(Error::Invalid("Teams service host"))?
                    .to_owned();
                (
                    url.to_owned(),
                    mime.to_owned(),
                    attachment
                        .pointer("/name")
                        .unwrap_or(&Value::Null)
                        .as_str()
                        .unwrap_or("attachment")
                        .to_owned(),
                    true,
                    vec![host],
                )
            } else {
                continue;
            };
            // Validate media before attaching credentials; the HTTP helper rechecks the URL.
            trusted_url(&url, &hosts)?;
            let bearer = if authenticated {
                Some(self.token().await?)
            } else {
                None
            };
            let trusted: Vec<&str> = hosts.iter().map(String::as_str).collect();
            let (headers, data) = self
                .http
                .download_response(&url, &trusted, bearer.as_deref(), remaining)
                .await?;
            remaining = remaining.checked_sub(data.len()).ok_or(Error::TooLarge)?;
            let mime = headers
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .unwrap_or(&mime)
                .to_owned();
            event.inbound.attachments.push(Attachment {
                filename: name,
                mime,
                size: Some(data.len() as u64),
                source: AttachmentSource::Bytes(data),
            });
        }
        let mut conversations = self.conversations.lock().await;
        let key = event.inbound.reply_channel.session_key();
        if conversations.len() >= 4096 && !conversations.contains_key(&key) {
            return Err(Error::Invalid("Teams conversation cache capacity"));
        }
        conversations.insert(key, service.into());
        Ok(event.inbound)
    }

    async fn route(&self, channel: &ChannelRef, message_id: Option<&str>) -> Result<String, Error> {
        if channel.platform != "teams" || channel.channel_id.is_empty() {
            return Err(Error::Invalid("Teams address"));
        }
        let service = self
            .conversations
            .lock()
            .await
            .get(&channel.session_key())
            .cloned()
            .ok_or(Error::Invalid("Teams verified conversation reference"))?;
        let mut url = self.service_url(&service)?;
        let mut path = url
            .path_segments_mut()
            .map_err(|_| Error::Invalid("Teams service URL"))?;
        path.pop_if_empty().extend([
            "v3",
            "conversations",
            channel.channel_id.as_str(),
            "activities",
        ]);
        if let Some(id) = message_id {
            if id.is_empty() {
                return Err(Error::Invalid("Teams activity id"));
            }
            path.push(id);
        }
        drop(path);
        #[cfg(test)]
        if let Some(base) = &self.connector_base {
            url = base
                .join(url.path())
                .map_err(|_| Error::Invalid("fixture Connector path"))?;
        }
        Ok(url.into())
    }

    async fn activity(
        &self,
        method: Method,
        url: &str,
        body: Option<Value>,
    ) -> Result<Value, Error> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let mut request = self.http.request(method.clone(), url).bearer_auth(token);
            if let Some(body) = &body {
                request = request.json(body);
            }
            let (status, _, bytes) = self.http.response(request).await?;
            if status == http::StatusCode::UNAUTHORIZED && attempt == 0 {
                self.tokens.invalidate().await;
                continue;
            }
            if !status.is_success() {
                return Err(Error::Status(status));
            }
            return if bytes.is_empty() {
                Ok(Value::Null)
            } else {
                Ok(serde_json::from_slice(&bytes)?)
            };
        }
        Err(Error::Authentication)
    }

    /// Send a reply quoting an original Teams activity and return its real id.
    pub async fn reply(&self, message: &MessageRef, text: &str) -> Result<MessageRef, Error> {
        self.send_activity(&message.channel, text, Some(&message.message_id))
            .await
    }
    async fn send_activity(
        &self,
        channel: &ChannelRef,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<MessageRef, Error> {
        if text.is_empty() || text.chars().count() > self.message_limit() {
            return Err(Error::Invalid("Teams text length"));
        }
        let url = self.route(channel, None).await?;
        let result=self.activity(Method::POST,&url,Some(json!({"type":"message","from":{"id":self.config.app_id},"text":text,"textFormat":"markdown","replyToId":reply_to.or(channel.thread_id.as_deref())}))).await?;
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: field(&result, "id")?.into(),
        })
    }
}

fn field<'a>(value: &'a Value, key: &'static str) -> Result<&'a str, Error> {
    value[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(Error::Invalid(key))
}
fn trusted_url(url: &str, hosts: &[String]) -> Result<Url, Error> {
    let url = Url::parse(url).map_err(|_| Error::Invalid("Teams HTTPS URL"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || url.fragment().is_some()
        || !url
            .host_str()
            .is_some_and(|host| hosts.iter().any(|allowed| allowed == host))
    {
        return Err(Error::Invalid("Teams untrusted endpoint"));
    }
    Ok(url)
}
fn file_mime(extension: &str) -> &'static str {
    match extension {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

impl Platform for Teams {
    fn bot_id(&self) -> &str {
        &self.config.app_id
    }
    fn receive(
        &self,
        request: WebhookRequest,
    ) -> WasmBoxedFuture<'_, Result<WebhookResponse, Error>> {
        Box::pin(async move {
            if request.method != Method::POST {
                return Err(Error::Authentication);
            }
            let token = request
                .headers
                .get("authorization")
                .and_then(|header| header.to_str().ok())
                .and_then(|header| header.strip_prefix("Bearer "))
                .ok_or(Error::Authentication)?;
            let activity: Value = serde_json::from_slice(&request.body)?;
            if activity
                .pointer("/channelId")
                .unwrap_or(&Value::Null)
                .as_str()
                != Some("msteams")
            {
                return Err(Error::Authentication);
            }
            let claims: Value = self.verifier.verify_endorsed(token, "msteams").await?;
            let service = field(&activity, "serviceUrl")?;
            if claims
                .pointer("/serviceurl")
                .unwrap_or(&Value::Null)
                .as_str()
                .or_else(|| {
                    claims
                        .pointer("/serviceUrl")
                        .unwrap_or(&Value::Null)
                        .as_str()
                })
                != Some(service)
            {
                return Err(Error::Authentication);
            }
            self.service_url(service)?;
            let mut response = WebhookResponse::ack();
            response.events = self.normalize(&activity)?.into_iter().collect();
            Ok(response)
        })
    }
    fn prepare(&self, event: Incoming) -> WasmBoxedFuture<'_, Result<Inbound, Error>> {
        Box::pin(self.prepare_event(event))
    }
}

impl ChatAdapter for Teams {
    fn platform(&self) -> &'static str {
        "teams"
    }
    fn message_limit(&self) -> usize {
        4000
    }
    fn supports_reactions(&self) -> bool {
        false
    }
    fn send<'a>(
        &'a self,
        channel: &'a ChannelRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>> {
        Box::pin(async move { Ok(self.send_activity(channel, text, None).await?) })
    }
    fn edit<'a>(
        &'a self,
        message: &'a MessageRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            if text.is_empty() || text.chars().count() > self.message_limit() {
                return Err(Error::Invalid("Teams text length").into());
            }
            let url = self
                .route(&message.channel, Some(&message.message_id))
                .await?;
            self.activity(Method::PUT,&url,Some(json!({"type":"message","from":{"id":self.config.app_id},"text":text,"textFormat":"markdown"}))).await?;
            Ok(())
        })
    }
    fn delete<'a>(&'a self, message: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            let url = self
                .route(&message.channel, Some(&message.message_id))
                .await?;
            self.activity(Method::DELETE, &url, None).await?;
            Ok(())
        })
    }
    fn add_reaction<'a>(
        &'a self,
        _message: &'a MessageRef,
        _emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("Teams bot reactions")) })
    }
    fn remove_reaction<'a>(
        &'a self,
        _message: &'a MessageRef,
        _emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("Teams bot reactions")) })
    }
}

#[cfg(test)]
mod tests;
