//! Google Chat ingress with pinned Google JWT verification and OAuth delivery.
//!
//! Configure an audience and inbound signer independently from outbound app
//! credentials. Replies preserve space and thread resource names.
//!
//! ```
//! use rig_messaging_platforms::googlechat::GoogleChatVerification;
//! let verification = GoogleChatVerification::Endpoint;
//! ```
use crate::{
    Error, Http, Incoming, Platform, TokenCache, WebhookRequest, WebhookResponse, auth::JwtVerifier,
};
use ::http::{Method, StatusCode};
use rig_core::wasm_compat::WasmBoxedFuture;
use rig_messaging::{
    Attachment, AttachmentSource, ChannelRef, ChatAdapter, ChatError, Inbound, MessageRef, Sender,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::Mutex, time::Instant};

const SCOPE: &str = "https://www.googleapis.com/auth/chat.bot";
const CHAT_SIGNER: &str = "chat@system.gserviceaccount.com";
/// Inbound authentication configured in the Google Chat console.
pub enum GoogleChatVerification {
    /// Endpoint URL audience with Google OIDC and the Chat service-account email.
    Endpoint,
    /// Project-number audience with the Chat service account's signing keys.
    ProjectNumber,
    /// Workspace add-on OIDC with the service-account email returned by getAuthorization.
    WorkspaceAddon { service_account: String },
}
/// App credentials used for outbound Chat API requests.
pub enum GoogleChatAuth {
    /// Explicit bearer token managed by the application.
    StaticToken(String),
    /// RSA service-account OAuth JWT exchange.
    ServiceAccount {
        email: String,
        private_key: String,
        token_url: String,
    },
    /// GCE metadata identity impersonating a distinct Chat app service account.
    Impersonation { target_service_account: String },
}
/// One Chat app's identity, authentication, and attachment policy.
pub struct GoogleChatConfig {
    /// Resource identity used for echo rejection and mentions, such as users/123.
    pub bot_id: String,
    /// Exact configured callback URL or project number.
    pub audience: String,
    /// Inbound signer mode.
    pub verification: GoogleChatVerification,
    /// Outbound OAuth mode.
    pub auth: GoogleChatAuth,
    /// REST base URL, normally https://chat.googleapis.com/v1.
    pub api_base: String,
    /// Maximum bytes across downloaded attachments for one event.
    pub media_limit: usize,
}
/// Authenticated Chat transport with per-space write pacing.
pub struct GoogleChat {
    config: GoogleChatConfig,
    http: Http,
    verifier: JwtVerifier,
    tokens: TokenCache,
    key: Option<jsonwebtoken::EncodingKey>,
    writes: Mutex<HashMap<String, Arc<Mutex<Instant>>>>,
    metadata_base: String,
    iam_base: String,
}
impl GoogleChat {
    /// Validate credentials and pin verification to Google's configured signer.
    pub fn new(config: GoogleChatConfig) -> Result<Self, Error> {
        if config.bot_id.is_empty() || config.audience.is_empty() || config.media_limit == 0 {
            return Err(Error::Invalid("Google Chat configuration"));
        }
        let http = Http::new(Duration::from_secs(30), config.media_limit.max(1024 * 1024))?;
        let (source, issuer) = match &config.verification {
            GoogleChatVerification::Endpoint | GoogleChatVerification::WorkspaceAddon { .. } => (
                "https://www.googleapis.com/oauth2/v3/certs".to_owned(),
                "https://accounts.google.com",
            ),
            GoogleChatVerification::ProjectNumber => {
                if !config.audience.chars().all(|c| c.is_ascii_digit()) {
                    return Err(Error::Invalid("Google Chat project audience"));
                }
                (
                    format!("https://www.googleapis.com/service_accounts/v1/jwk/{CHAT_SIGNER}"),
                    CHAT_SIGNER,
                )
            }
        };
        if let GoogleChatVerification::WorkspaceAddon { service_account } = &config.verification {
            validate_account(service_account)?;
        }
        let key = match &config.auth {
            GoogleChatAuth::ServiceAccount {
                email, private_key, ..
            } => {
                validate_account(email)?;
                Some(
                    jsonwebtoken::EncodingKey::from_rsa_pem(private_key.as_bytes())
                        .map_err(|_| Error::Invalid("Google Chat RSA key"))?,
                )
            }
            GoogleChatAuth::StaticToken(token) => {
                if token.is_empty() {
                    return Err(Error::Invalid("Google Chat access token"));
                }
                None
            }
            GoogleChatAuth::Impersonation {
                target_service_account,
            } => {
                validate_account(target_service_account)?;
                None
            }
        };
        let mut verifier = JwtVerifier::new(http.clone(), source, issuer, &config.audience)?;
        if !matches!(config.verification, GoogleChatVerification::ProjectNumber) {
            verifier =
                verifier.with_issuers(&["https://accounts.google.com", "accounts.google.com"])?;
        }
        Ok(Self {
            config,
            http,
            verifier,
            tokens: TokenCache::new(),
            key,
            writes: Mutex::new(HashMap::new()),
            metadata_base: "http://metadata.google.internal".into(),
            iam_base: "https://iamcredentials.googleapis.com/v1".into(),
        })
    }
    async fn token(&self) -> Result<String, Error> {
        if let GoogleChatAuth::StaticToken(token) = &self.config.auth {
            return Ok(token.clone());
        }
        self.tokens.get_or_refresh(||async {
            match &self.config.auth {
                GoogleChatAuth::StaticToken(_) => Err(Error::Invalid("Google Chat token mode")),
                GoogleChatAuth::ServiceAccount {email,token_url,..} => {
                    let now=SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_|Error::Invalid("system clock"))?.as_secs();
                    let claims=json!({"iss":email,"scope":SCOPE,"aud":token_url,"iat":now,"exp":now+3600});
                    let key=self.key.as_ref().ok_or(Error::Authentication)?;
                    let assertion=jsonwebtoken::encode(&jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),&claims,key).map_err(|_|Error::Authentication)?;
                    let response:Value=self.http.json(self.http.request(Method::POST,token_url).form(&[("grant_type","urn:ietf:params:oauth:grant-type:jwt-bearer"),("assertion",assertion.as_str())])).await?;
                    Ok((required(&response,"access_token")?.into(),Duration::from_secs(response.pointer("/expires_in").unwrap_or(&Value::Null).as_u64().ok_or(Error::Invalid("Google Chat token expiry"))?)))
                },
                GoogleChatAuth::Impersonation {target_service_account} => self.impersonated_token(target_service_account).await,
            }
        }).await
    }
    async fn impersonated_token(&self, target: &str) -> Result<(String, Duration), Error> {
        let client = rig_reqwest::reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(rig_reqwest::reqwest::redirect::Policy::none())
            .no_proxy()
            .build()?;
        let email = client
            .get(format!(
                "{}/computeMetadata/v1/instance/service-accounts/default/email",
                self.metadata_base
            ))
            .header("Metadata-Flavor", "Google")
            .send()
            .await?;
        if !email.status().is_success() {
            return Err(Error::Status(email.status()));
        }
        if email
            .headers()
            .get("Metadata-Flavor")
            .and_then(|v| v.to_str().ok())
            != Some("Google")
        {
            return Err(Error::Authentication);
        }
        if email.content_length().is_some_and(|n| n > 1024) {
            return Err(Error::TooLarge);
        }
        let mut email = email;
        let mut email_bytes = Vec::new();
        while let Some(chunk) = email.chunk().await? {
            if chunk.len() > 1024usize.saturating_sub(email_bytes.len()) {
                return Err(Error::TooLarge);
            }
            email_bytes.extend_from_slice(&chunk);
        }
        let email = email_bytes;
        if email.len() > 1024 {
            return Err(Error::TooLarge);
        }
        if std::str::from_utf8(&email)
            .map_err(|_| Error::Invalid("metadata email"))?
            .trim()
            == target
        {
            return Err(Error::Invalid("Google Chat self impersonation"));
        }
        let mut response = client
            .get(format!(
                "{}/computeMetadata/v1/instance/service-accounts/default/token",
                self.metadata_base
            ))
            .header("Metadata-Flavor", "Google")
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status()));
        }
        if response
            .headers()
            .get("Metadata-Flavor")
            .and_then(|v| v.to_str().ok())
            != Some("Google")
        {
            return Err(Error::Authentication);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > 16_384usize.saturating_sub(bytes.len()) {
                return Err(Error::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        let response: Value = serde_json::from_slice(&bytes)?;
        let base = required(&response, "access_token")?;
        let endpoint = format!(
            "{}/projects/-/serviceAccounts/{target}:generateAccessToken",
            self.iam_base
        );
        let response: Value = self
            .http
            .json(
                self.http
                    .request(Method::POST, &endpoint)
                    .bearer_auth(base)
                    .json(&json!({"scope":[SCOPE],"lifetime":"3600s"})),
            )
            .await?;
        let expiry = required(&response, "expireTime")?
            .parse::<chrono::DateTime<chrono::Utc>>()
            .map_err(|_| Error::Invalid("impersonated token expiry"))?;
        let remaining = expiry
            .signed_duration_since(chrono::Utc::now())
            .to_std()
            .map_err(|_| Error::Authentication)?;
        Ok((required(&response, "accessToken")?.into(), remaining))
    }
    fn channel(&self, channel: &ChannelRef) -> Result<(), Error> {
        if channel.platform != "googlechat"
            || channel
                .scope_id
                .as_deref()
                .is_some_and(|scope| scope != self.config.audience)
        {
            return Err(Error::Invalid("Google Chat channel identity"));
        }
        space_name(&channel.channel_id)?;
        self.url(&channel.channel_id)?;
        if let Some(thread) = &channel.thread_id {
            let prefix = format!("{}/threads/", channel.channel_id);
            if thread
                .strip_prefix(&prefix)
                .is_none_or(|id| id.is_empty() || id.contains('/'))
            {
                return Err(Error::Invalid("Google Chat thread identity"));
            }
            self.url(thread)?;
        }
        Ok(())
    }
    fn url(&self, resource: &str) -> Result<String, Error> {
        if resource.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part
                    .chars()
                    .any(|c| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.'))
        }) {
            return Err(Error::Invalid("Google Chat resource"));
        }
        Ok(format!(
            "{}/{resource}",
            self.config.api_base.trim_end_matches('/')
        ))
    }
    async fn write(
        &self,
        space: &str,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Value, Error> {
        let lock = {
            let mut writes = self.writes.lock().await;
            writes
                .entry(space.into())
                .or_insert_with(|| Arc::new(Mutex::new(Instant::now())))
                .clone()
        };
        let mut next = lock.lock().await;
        tokio::time::sleep_until(*next).await;
        for attempt in 0..2 {
            let token = self.token().await?;
            let mut request = self.http.request(method.clone(), url).bearer_auth(token);
            if let Some(body) = body {
                request = request.json(body);
            }
            let (status, headers, bytes) = self.http.response(request).await?;
            *next = Instant::now() + Duration::from_secs(1);
            if status == StatusCode::UNAUTHORIZED
                && attempt == 0
                && !matches!(self.config.auth, GoogleChatAuth::StaticToken(_))
            {
                self.tokens.invalidate().await;
                tokio::time::sleep_until(*next).await;
                continue;
            }
            if status == StatusCode::TOO_MANY_REQUESTS {
                let delay = headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1)
                    .min(60);
                *next = Instant::now() + Duration::from_secs(delay);
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
}
fn validate_account(email: &str) -> Result<(), Error> {
    if !email.ends_with(".gserviceaccount.com")
        || !email.contains('@')
        || email
            .chars()
            .any(|c| !c.is_ascii_alphanumeric() && !matches!(c, '@' | '.' | '-' | '_'))
    {
        return Err(Error::Invalid("Google service account"));
    }
    Ok(())
}
fn required<'a>(value: &'a Value, name: &str) -> Result<&'a str, Error> {
    value[name]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or(Error::Invalid("Google Chat required field"))
}
fn space_name(name: &str) -> Result<(), Error> {
    if name
        .strip_prefix("spaces/")
        .is_none_or(|id| id.is_empty() || id.contains('/'))
    {
        return Err(Error::Invalid("Google Chat space name"));
    }
    Ok(())
}
fn message_name(name: &str, space: &str) -> Result<(), Error> {
    let prefix = format!("{space}/messages/");
    if name
        .strip_prefix(&prefix)
        .is_none_or(|id| id.is_empty() || id.contains('/'))
    {
        return Err(Error::Invalid("Google Chat message name"));
    }
    Ok(())
}
impl Platform for GoogleChat {
    fn bot_id(&self) -> &str {
        &self.config.bot_id
    }
    fn receive(
        &self,
        request: WebhookRequest,
    ) -> WasmBoxedFuture<'_, Result<WebhookResponse, Error>> {
        Box::pin(async move {
            if request.method != Method::POST {
                return Err(Error::Invalid("Google Chat callback method"));
            }
            let token = request
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .ok_or(Error::Authentication)?;
            let claims: Value = self.verifier.verify(token).await?;
            match &self.config.verification {
                GoogleChatVerification::ProjectNumber => {}
                verification => {
                    let email = match verification {
                        GoogleChatVerification::WorkspaceAddon { service_account } => {
                            service_account.as_str()
                        }
                        _ => CHAT_SIGNER,
                    };
                    if claims.pointer("/email").unwrap_or(&Value::Null) != email
                        || claims.pointer("/email_verified").unwrap_or(&Value::Null) != true
                    {
                        return Err(Error::Authentication);
                    }
                }
            }
            let event: Value = serde_json::from_slice(&request.body)?;
            let mut response = WebhookResponse::ack();
            response.content_type = "application/json";
            response.body = bytes::Bytes::from_static(b"{}");
            let wrapped = event.get("chat");
            let message = wrapped
                .and_then(|chat| chat.pointer("/messagePayload/message"))
                .or_else(|| event.get("message"));
            let Some(message) = message else {
                return Ok(response);
            };
            let space = message
                .get("space")
                .or_else(|| wrapped.and_then(|chat| chat.pointer("/messagePayload/space")))
                .or_else(|| event.get("space"))
                .ok_or(Error::Invalid("Google Chat space"))?;
            let space_id = required(space, "name")?;
            space_name(space_id)?;
            let name = required(message, "name")?;
            message_name(name, space_id)?;
            let sender = message
                .get("sender")
                .or_else(|| wrapped.and_then(|chat| chat.get("user")))
                .or_else(|| event.get("user"))
                .ok_or(Error::Invalid("Google Chat sender"))?;
            let sender_id = required(sender, "name")?;
            let thread = message
                .pointer("/thread/name")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if thread
                .as_ref()
                .is_some_and(|thread| !thread.starts_with(&format!("{space_id}/threads/")))
            {
                return Err(Error::Invalid("Google Chat thread"));
            }
            let channel = ChannelRef {
                platform: "googlechat".into(),
                scope_id: Some(self.config.audience.clone()),
                channel_id: space_id.into(),
                thread_id: thread,
            };
            let mention = message
                .pointer("/annotations")
                .unwrap_or(&Value::Null)
                .as_array()
                .is_some_and(|annotations| {
                    annotations.iter().any(|annotation| {
                        annotation
                            .pointer("/userMention/user/name")
                            .and_then(Value::as_str)
                            == Some(self.config.bot_id.as_str())
                    })
                });
            let inbound = Inbound {
                message: MessageRef {
                    channel: channel.clone(),
                    message_id: name.into(),
                },
                reply_channel: channel,
                sender: Sender {
                    id: sender_id.into(),
                    name: sender
                        .pointer("/displayName")
                        .unwrap_or(&Value::Null)
                        .as_str()
                        .unwrap_or(sender_id)
                        .into(),
                    is_bot: sender.pointer("/type").unwrap_or(&Value::Null) == "BOT",
                },
                text: message
                    .pointer("/argumentText")
                    .unwrap_or(&Value::Null)
                    .as_str()
                    .or_else(|| message.pointer("/text").unwrap_or(&Value::Null).as_str())
                    .unwrap_or_default()
                    .into(),
                attachments: Vec::new(),
                is_dm: space.pointer("/type").unwrap_or(&Value::Null) == "DM"
                    || space.pointer("/spaceType").unwrap_or(&Value::Null) == "DIRECT_MESSAGE",
                is_thread: message
                    .pointer("/threadReply")
                    .unwrap_or(&Value::Null)
                    .as_bool()
                    .unwrap_or(false),
                mentions_bot: mention,
            };
            response.events.push(Incoming {
                inbound,
                payload: message.clone(),
            });
            Ok(response)
        })
    }
    fn prepare(&self, mut event: Incoming) -> WasmBoxedFuture<'_, Result<Inbound, Error>> {
        Box::pin(async move {
            let Some(attachments) = event
                .payload
                .pointer("/attachment")
                .unwrap_or(&Value::Null)
                .as_array()
            else {
                return Ok(event.inbound);
            };
            let mut remaining = self.config.media_limit;
            for attachment in attachments {
                if attachment.pointer("/source").unwrap_or(&Value::Null) == "DRIVE_FILE" {
                    event
                        .inbound
                        .text
                        .push_str("\n[Google Drive attachment requires user authorization]");
                    continue;
                }
                let Some(resource) = attachment
                    .pointer("/attachmentDataRef/resourceName")
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                if !resource.starts_with(&format!("{}/", event.inbound.message.channel.channel_id))
                {
                    return Err(Error::Invalid("Google Chat attachment resource"));
                }
                self.url(resource)?;
                let token = self.token().await?;
                let url = format!(
                    "https://chat.googleapis.com/v1/media/{}?alt=media",
                    resource
                );
                let (headers, body) = self
                    .http
                    .download_response(&url, &["chat.googleapis.com"], Some(&token), remaining)
                    .await?;
                remaining = remaining.saturating_sub(body.len());
                let mime = headers
                    .get(::http::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.split(';').next())
                    .unwrap_or("application/octet-stream");
                event.inbound.attachments.push(Attachment {
                    filename: attachment
                        .pointer("/contentName")
                        .unwrap_or(&Value::Null)
                        .as_str()
                        .unwrap_or("attachment")
                        .into(),
                    mime: mime.into(),
                    size: Some(body.len() as u64),
                    source: AttachmentSource::Bytes(body),
                });
            }
            Ok(event.inbound)
        })
    }
}
impl ChatAdapter for GoogleChat {
    fn platform(&self) -> &'static str {
        "googlechat"
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
        Box::pin(async move {
            self.channel(channel)?;
            let mut body = json!({"text":text});
            if text.chars().count() > self.message_limit() {
                return Err(Error::Invalid("Google Chat text length").into());
            }
            let mut url = self.url(&format!("{}/messages", channel.channel_id))?;
            if let Some(thread) = &channel.thread_id {
                body.as_object_mut()
                    .ok_or(Error::Invalid("Google Chat message object"))?
                    .insert("thread".into(), json!({"name":thread}));
                url.push_str("?messageReplyOption=REPLY_MESSAGE_OR_FAIL");
            }
            if serde_json::to_vec(&body).map_err(Error::from)?.len() > 32_000 {
                return Err(Error::TooLarge.into());
            }
            let response = self
                .write(&channel.channel_id, Method::POST, &url, Some(&body))
                .await?;
            let name = required(&response, "name")?;
            message_name(name, &channel.channel_id)?;
            Ok(MessageRef {
                channel: channel.clone(),
                message_id: name.into(),
            })
        })
    }
    fn edit<'a>(
        &'a self,
        message: &'a MessageRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.channel(&message.channel)?;
            message_name(&message.message_id, &message.channel.channel_id)?;
            if text.chars().count() > self.message_limit() {
                return Err(Error::Invalid("Google Chat text length").into());
            }
            let url = format!("{}?updateMask=text", self.url(&message.message_id)?);
            self.write(
                &message.channel.channel_id,
                Method::PATCH,
                &url,
                Some(&json!({"text":text})),
            )
            .await?;
            Ok(())
        })
    }
    fn delete<'a>(&'a self, message: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.channel(&message.channel)?;
            message_name(&message.message_id, &message.channel.channel_id)?;
            self.write(
                &message.channel.channel_id,
                Method::DELETE,
                &self.url(&message.message_id)?,
                None,
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
        Box::pin(async { Err(ChatError::Unsupported("Google Chat bot reactions")) })
    }
    fn remove_reaction<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("Google Chat bot reactions")) })
    }
}
#[cfg(test)]
mod tests;
