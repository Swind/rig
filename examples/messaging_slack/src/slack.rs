//! Slack event normalization, Socket Mode ingress and outbound Web API calls.
//!
//! Admission precedes authenticated attachment downloads. Socket envelopes are
//! acknowledged before spawning agent turns.

use rig_core::wasm_compat::{WasmBoxedFuture, WasmCompatSend, WasmCompatSync};
use rig_http::{
    http_client::{NoBody, Request},
    ws_client::{ConnectOptions, Frame, WebSocketClientExt, WebSocketConnection},
};
use rig_messaging::{
    Attachment, AttachmentSource, ChannelRef, ChatAdapter, ChatError, ChatRouter, Inbound,
    MessageRef, Sender,
};
use rig_reqwest::reqwest;
use serde_json::{Value, json};
use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};

const LIMIT: usize = 11_900;
const ATTACHMENT_LIMIT: usize = 10 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Socket(#[from] rig_http::http_client::Error),
    #[error(transparent)]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("Slack API {method}: {code}")]
    Api { method: String, code: String },
    #[error("Slack response lacks {0}")]
    Missing(&'static str),
    #[error("Slack attachment exceeds the 10 MiB total limit")]
    AttachmentLimit,
}
fn platform(
    error: impl std::error::Error + WasmCompatSend + WasmCompatSync + 'static,
) -> ChatError {
    ChatError::Platform(Box::new(error))
}
fn field<'a>(value: &'a Value, name: &'static str) -> Result<&'a str, Error> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(Error::Missing(name))
}

pub(crate) struct SlackAdapter {
    client: reqwest::Client,
    token: String,
    api: String,
}
impl SlackAdapter {
    pub(crate) fn new(token: String) -> Result<Self, Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
            token,
            api: "https://slack.com/api".into(),
        })
    }
    async fn api(&self, method: &str, body: &Value, token: &str) -> Result<Value, Error> {
        let request = self
            .client
            .post(format!("{}/{method}", self.api))
            .bearer_auth(token);
        let request = if method == "apps.connections.open" {
            request
                .header("content-type", "application/x-www-form-urlencoded")
                .body("")
        } else {
            request.json(body)
        };
        let result: Value = request.send().await?.error_for_status()?.json().await?;
        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(Error::Api {
                method: method.into(),
                code: result
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown_error")
                    .into(),
            });
        }
        Ok(result)
    }
    pub(crate) async fn identity(&self) -> Result<Identity, Error> {
        let result = self.api("auth.test", &json!({}), &self.token).await?;
        Ok(Identity {
            bot: field(&result, "user_id")?.into(),
            team: field(&result, "team_id")?.into(),
        })
    }
    async fn message(&self, method: &str, mut body: Value) -> Result<Value, Error> {
        match self.api(method, &body, &self.token).await {
            Err(Error::Api { code, .. })
                if matches!(code.as_str(), "invalid_blocks" | "msg_blocks_too_long") =>
            {
                // Unsupported markdown blocks can degrade to the accessibility text.
                if let Some(object) = body.as_object_mut() {
                    object.remove("blocks");
                    object.insert("mrkdwn".into(), json!(false));
                }
                self.api(method, &body, &self.token).await
            }
            result => result,
        }
    }
    async fn download(&self, url: &str, remaining: usize) -> Result<Vec<u8>, Error> {
        let mut response = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()?;
        if response
            .content_length()
            .is_some_and(|n| n > remaining as u64)
        {
            return Err(Error::AttachmentLimit);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > remaining.saturating_sub(bytes.len()) {
                return Err(Error::AttachmentLimit);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}
fn message_body(channel: &str, text: &str) -> Value {
    json!({"channel":channel,"text":text,"blocks":[{"type":"markdown","text":text}],"unfurl_links":false,"unfurl_media":false,"parse":"none","link_names":false})
}
fn emoji_name(emoji: &str) -> &str {
    match emoji {
        "👀" => "eyes",
        "🤔" => "thinking_face",
        "🔥" => "fire",
        "👨‍💻" => "technologist",
        "⚡" => "zap",
        "🆗" => "ok",
        "😱" => "scream",
        "🥱" => "yawning_face",
        "😨" => "fearful",
        "😊" => "blush",
        "😎" => "sunglasses",
        "🫡" => "saluting_face",
        "🤓" => "nerd_face",
        "😏" => "smirk",
        "✌️" => "v",
        "💪" => "muscle",
        "🦾" => "mechanical_arm",
        other => other.trim_matches(':'),
    }
}
impl ChatAdapter for SlackAdapter {
    fn platform(&self) -> &'static str {
        "slack"
    }
    fn message_limit(&self) -> usize {
        LIMIT
    }
    fn renders_native_tables(&self) -> bool {
        true
    }
    fn send<'a>(
        &'a self,
        ch: &'a ChannelRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>> {
        Box::pin(async move {
            let mut body = message_body(&ch.channel_id, text);
            if let (Some(thread), Some(object)) = (&ch.thread_id, body.as_object_mut()) {
                object.insert("thread_ts".into(), json!(thread));
            }
            let result = self
                .message("chat.postMessage", body)
                .await
                .map_err(platform)?;
            Ok(MessageRef {
                channel: ch.clone(),
                message_id: field(&result, "ts").map_err(platform)?.into(),
            })
        })
    }
    fn edit<'a>(
        &'a self,
        msg: &'a MessageRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            let mut body = message_body(&msg.channel.channel_id, text);
            if let Some(object) = body.as_object_mut() {
                object.insert("ts".into(), json!(msg.message_id));
            }
            self.message("chat.update", body).await.map_err(platform)?;
            Ok(())
        })
    }
    fn delete<'a>(&'a self, msg: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.api(
                "chat.delete",
                &json!({"channel":msg.channel.channel_id,"ts":msg.message_id}),
                &self.token,
            )
            .await
            .map_err(platform)?;
            Ok(())
        })
    }
    fn add_reaction<'a>(
        &'a self,
        msg: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.api("reactions.add", &json!({"channel":msg.channel.channel_id,"timestamp":msg.message_id,"name":emoji_name(emoji)}), &self.token).await.map_err(platform)?;
            Ok(())
        })
    }
    fn remove_reaction<'a>(
        &'a self,
        msg: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.api("reactions.remove", &json!({"channel":msg.channel.channel_id,"timestamp":msg.message_id,"name":emoji_name(emoji)}), &self.token).await.map_err(platform)?;
            Ok(())
        })
    }
}

#[derive(Clone)]
pub(crate) struct Identity {
    bot: String,
    team: String,
}
fn normalize(payload: &Value, identity: &Identity) -> Option<Inbound> {
    if payload.get("team_id")?.as_str()? != identity.team {
        return None;
    }
    let event = payload.get("event")?;
    if !matches!(event.get("type")?.as_str()?, "message" | "app_mention") {
        return None;
    }
    if !matches!(
        event.get("subtype").and_then(Value::as_str).unwrap_or(""),
        "" | "file_share" | "me_message" | "thread_broadcast" | "bot_message"
    ) {
        return None;
    }
    let channel = field(event, "channel").ok()?;
    let ts = field(event, "ts").ok()?;
    let user = field(event, "user").ok()?;
    let text = event.get("text").and_then(Value::as_str).unwrap_or("");
    let thread = event
        .get("thread_ts")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let original = ChannelRef {
        platform: "slack".into(),
        scope_id: Some(identity.team.clone()),
        channel_id: channel.into(),
        thread_id: thread.clone(),
    };
    let mut reply = original.clone();
    let is_dm =
        event.get("channel_type").and_then(Value::as_str) == Some("im") || channel.starts_with('D');
    if !is_dm && reply.thread_id.is_none() {
        reply.thread_id = Some(ts.into());
    }
    let mention = format!("<@{}>", identity.bot);
    Some(Inbound {
        message: MessageRef {
            channel: original,
            message_id: ts.into(),
        },
        reply_channel: reply,
        sender: Sender {
            id: user.into(),
            name: user.into(),
            is_bot: event.get("bot_id").is_some()
                || event.get("subtype").and_then(Value::as_str) == Some("bot_message"),
        },
        text: text.replace(&mention, "").trim().into(),
        attachments: vec![],
        is_dm,
        is_thread: thread.is_some(),
        mentions_bot: text.contains(&mention),
    })
}
fn trusted_file(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| {
        url.scheme() == "https"
            && url
                .host_str()
                .is_some_and(|host| host == "slack.com" || host.ends_with(".slack.com"))
    })
}

#[derive(Clone)]
pub(crate) struct Handler {
    adapter: Arc<SlackAdapter>,
    router: Arc<ChatRouter>,
    identity: Identity,
}
impl Handler {
    pub(crate) fn new(
        adapter: Arc<SlackAdapter>,
        router: Arc<ChatRouter>,
        identity: Identity,
    ) -> Self {
        Self {
            adapter,
            router,
            identity,
        }
    }
    async fn process(&self, payload: Value) -> Result<(), ChatError> {
        let Some(mut input) = normalize(&payload, &self.identity) else {
            return Ok(());
        };
        if !self.router.allows(&input, &self.identity.bot) {
            return Ok(());
        }
        let mut used = 0;
        if let Some(files) = payload
            .get("event")
            .and_then(|v| v.get("files"))
            .and_then(Value::as_array)
        {
            for file in files {
                let filename = file.get("name").and_then(Value::as_str).unwrap_or("file");
                let url = file
                    .get("url_private_download")
                    .or_else(|| file.get("url_private"))
                    .and_then(Value::as_str);
                let size = file.get("size").and_then(Value::as_u64);
                let bytes = match url {
                    Some(url)
                        if trusted_file(url)
                            && size.is_none_or(|n| n <= (ATTACHMENT_LIMIT - used) as u64) =>
                    {
                        self.adapter
                            .download(url, ATTACHMENT_LIMIT - used)
                            .await
                            .ok()
                    }
                    _ => None,
                };
                if let Some(bytes) = bytes {
                    used += bytes.len();
                    input.attachments.push(Attachment {
                        filename: filename.into(),
                        mime: file
                            .get("mimetype")
                            .and_then(Value::as_str)
                            .unwrap_or("application/octet-stream")
                            .into(),
                        size: Some(bytes.len() as u64),
                        source: AttachmentSource::Bytes(bytes.into()),
                    });
                } else {
                    input
                        .text
                        .push_str(&format!("\n[Attachment unavailable: {filename}]"));
                }
            }
        }
        self.router
            .handle(self.adapter.clone(), input, &self.identity.bot)
            .await
    }
    async fn socket(
        &self,
        socket: &mut dyn WebSocketConnection,
        recent: &mut Recent,
    ) -> Result<(), Error> {
        loop {
            let frame = match tokio::time::timeout(Duration::from_secs(45), socket.recv()).await {
                Ok(result) => result?,
                Err(_) => {
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        socket.send(Frame::Ping(Default::default())),
                    )
                    .await??;
                    tokio::time::timeout(Duration::from_secs(45), socket.recv()).await??
                }
            };
            match frame {
                Some(Frame::Text(text)) => {
                    let envelope: Value = serde_json::from_str(&text)?;
                    if let Some(id) = envelope.get("envelope_id").and_then(Value::as_str) {
                        tokio::time::timeout(
                            Duration::from_secs(10),
                            socket.send(Frame::Text(json!({"envelope_id":id}).to_string())),
                        )
                        .await??;
                    }
                    if envelope.get("type").and_then(Value::as_str) == Some("disconnect") {
                        return Ok(());
                    }
                    if envelope.get("type").and_then(Value::as_str) != Some("events_api") {
                        continue;
                    }
                    let Some(payload) = envelope.get("payload") else {
                        continue;
                    };
                    let Some(input) = normalize(payload, &self.identity) else {
                        continue;
                    };
                    let key = (
                        input.message.channel.session_key(),
                        input.message.message_id,
                    );
                    if !recent.insert(key) {
                        continue;
                    }
                    let handler = self.clone();
                    let payload = payload.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handler.process(payload).await {
                            eprintln!("Slack turn failed: {error}");
                        }
                    });
                }
                Some(Frame::Ping(bytes)) => {
                    tokio::time::timeout(Duration::from_secs(10), socket.send(Frame::Pong(bytes)))
                        .await??;
                }
                Some(Frame::Close(_)) | None => return Ok(()),
                _ => {}
            }
        }
    }
    pub(crate) async fn run(&self, app_token: &str) -> Result<(), Error> {
        let mut delay = 1;
        let mut recent = Recent::default();
        loop {
            let connection = async {
                let response = self
                    .adapter
                    .api("apps.connections.open", &json!({}), app_token)
                    .await?;
                let url = field(&response, "url")?;
                let request = Request::builder()
                    .uri(url)
                    .body(NoBody)
                    .map_err(rig_http::http_client::Error::from)?;
                let mut socket = rig_tungstenite::TungsteniteClient::new()
                    .connect(
                        request,
                        ConnectOptions::new().with_timeout(Some(Duration::from_secs(15))),
                    )
                    .await?;
                delay = 1;
                self.socket(socket.as_mut(), &mut recent).await
            }
            .await;
            if let Err(error) = connection {
                eprintln!("Slack connection ended: {error}");
            }
            tokio::time::sleep(Duration::from_secs(delay)).await;
            delay = (delay * 2).min(30);
        }
    }
}
#[derive(Default)]
struct Recent {
    entries: HashSet<(String, String)>,
    order: VecDeque<(String, String)>,
}
impl Recent {
    fn insert(&mut self, key: (String, String)) -> bool {
        if !self.entries.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > 1024
            && let Some(old) = self.order.pop_front()
        {
            self.entries.remove(&old);
        }
        true
    }
}

#[cfg(test)]
mod tests;
