#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;

fn identity() -> Identity {
    Identity {
        bot: "UBOT".into(),
        team: "T1".into(),
    }
}
fn payload(channel: &str, ts: &str, thread: Option<&str>) -> Value {
    let mut event =
        json!({"type":"message","channel":channel,"user":"U1","text":"<@UBOT> hello","ts":ts});
    if let (Some(thread), Some(object)) = (thread, event.as_object_mut()) {
        object.insert("thread_ts".into(), json!(thread));
    }
    json!({"team_id":"T1","event":event})
}
#[test]
fn normalization_preserves_reactions_and_shares_thread_identity() -> Result<(), Error> {
    let first =
        normalize(&payload("C1", "100.1", None), &identity()).ok_or(Error::Missing("input"))?;
    let second = normalize(&payload("C1", "100.2", Some("100.1")), &identity())
        .ok_or(Error::Missing("input"))?;
    assert_eq!(
        first.reply_channel.session_key(),
        second.reply_channel.session_key()
    );
    assert_eq!(first.message.message_id, "100.1");
    assert!(first.message.channel.thread_id.is_none());
    assert!(!first.is_thread);
    assert!(second.is_thread);
    assert_eq!(first.text, "hello");
    let dm =
        normalize(&payload("D1", "100.1", None), &identity()).ok_or(Error::Missing("input"))?;
    assert!(dm.is_dm);
    assert!(dm.reply_channel.thread_id.is_none());
    assert_ne!(
        first.reply_channel.session_key(),
        dm.reply_channel.session_key()
    );
    Ok(())
}
#[test]
fn foreign_workspace_and_non_messages_are_ignored() {
    let mut event = payload("C1", "100", None);
    event["team_id"] = json!("T2");
    assert!(normalize(&event, &identity()).is_none());
    event["team_id"] = json!("T1");
    for subtype in ["message_changed", "message_deleted", "channel_join"] {
        event["event"]["subtype"] = json!(subtype);
        assert!(normalize(&event, &identity()).is_none());
    }
    event["event"]["subtype"] = json!("file_share");
    assert!(normalize(&event, &identity()).is_some());
}
#[test]
fn emoji_and_download_hosts_are_validated() {
    for emoji in [
        "👀",
        "🤔",
        "🔥",
        "👨‍💻",
        "⚡",
        "🆗",
        "😱",
        "🥱",
        "😨",
        "😊",
        "😎",
        "🫡",
        "🤓",
        "😏",
        "✌️",
        "💪",
        "🦾",
    ] {
        assert!(emoji_name(emoji).is_ascii());
    }
    assert_eq!(emoji_name(":custom:"), "custom");
    assert!(trusted_file("https://files.slack.com/files/a"));
    for url in [
        "http://files.slack.com/a",
        "https://slack.com.evil.test/a",
        "https://evilslack.com/a",
        "invalid",
    ] {
        assert!(!trusted_file(url));
    }
}
#[test]
fn deduplication_is_bounded_and_uses_channel_identity() {
    let mut recent = Recent::default();
    assert!(recent.insert(("C1".into(), "1".into())));
    assert!(!recent.insert(("C1".into(), "1".into())));
    assert!(recent.insert(("C2".into(), "1".into())));
    for i in 2..=1025 {
        assert!(recent.insert(("C1".into(), i.to_string())));
    }
    assert_eq!(recent.entries.len(), 1024);
    assert!(recent.insert(("C1".into(), "1".into())));
}

async fn server(
    responses: Vec<(String, bool)>,
) -> std::io::Result<(
    String,
    tokio::task::JoinHandle<std::io::Result<Vec<(String, String, String)>>>,
)> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = format!("http://{}", listener.local_addr()?);
    let handle = tokio::spawn(async move {
        let mut requests = vec![];
        for (body, length) in responses {
            let (socket, _) = listener.accept().await?;
            let mut reader = tokio::io::BufReader::new(socket);
            let mut first = String::new();
            reader.read_line(&mut first).await?;
            let mut content_length = 0;
            let mut auth = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await?;
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((key, value)) = line.split_once(':') {
                    if key.eq_ignore_ascii_case("content-length") {
                        content_length = value
                            .trim()
                            .parse::<usize>()
                            .map_err(std::io::Error::other)?;
                    }
                    if key.eq_ignore_ascii_case("authorization") {
                        auth = value.trim().into();
                    }
                }
            }
            let mut bytes = vec![0; content_length];
            reader.read_exact(&mut bytes).await?;
            requests.push((first, String::from_utf8_lossy(&bytes).into(), auth));
            let header = if length {
                format!("Content-Length: {}\r\n", body.len())
            } else {
                String::new()
            };
            reader.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{header}Connection: close\r\n\r\n{body}").as_bytes()).await?;
            reader.get_mut().shutdown().await?;
        }
        Ok(requests)
    });
    Ok((address, handle))
}
fn adapter(api: String) -> Result<SlackAdapter, Error> {
    let mut adapter = SlackAdapter::new("offline-bot-token".into())?;
    adapter.api = api;
    Ok(adapter)
}

#[tokio::test]
async fn outbound_http_uses_reply_thread_original_reactions_and_block_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    let (url, server) = server(
        vec![
            (
                json!({"ok":false,"error":"invalid_blocks"}).to_string(),
                true,
            ),
            (json!({"ok":true,"ts":"200"}).to_string(), true),
        ]
        .into_iter()
        .chain((0..4).map(|_| (json!({"ok":true}).to_string(), true)))
        .collect(),
    )
    .await?;
    let adapter = adapter(url)?;
    let input =
        normalize(&payload("C1", "100", None), &identity()).ok_or(Error::Missing("input"))?;
    let sent = adapter
        .send(&input.reply_channel, "| a | b |\n|---|---|\n| c | d |")
        .await?;
    adapter.edit(&sent, "final").await?;
    adapter.delete(&sent).await?;
    adapter.add_reaction(&input.message, "🤔").await?;
    adapter.remove_reaction(&input.message, "🤔").await?;
    let requests = tokio::time::timeout(Duration::from_secs(5), server).await???;
    assert_eq!(requests.len(), 6);
    for (_, _, auth) in &requests {
        assert_eq!(auth, "Bearer offline-bot-token");
    }
    let body: Value = serde_json::from_str(&requests[0].1)?;
    assert_eq!(body["thread_ts"], "100");
    assert_eq!(body["blocks"][0]["type"], "markdown");
    assert!(
        body["blocks"][0]["text"]
            .as_str()
            .is_some_and(|s| s.contains("| c | d |"))
    );
    let fallback: Value = serde_json::from_str(&requests[1].1)?;
    assert!(fallback.get("blocks").is_none());
    assert_eq!(fallback["mrkdwn"], false);
    assert!(requests[2].0.starts_with("POST /chat.update "));
    assert!(requests[3].0.starts_with("POST /chat.delete "));
    for (request, body, _) in &requests[4..] {
        assert!(request.contains("/reactions."));
        let body: Value = serde_json::from_str(body)?;
        assert_eq!(body["channel"], "C1");
        assert_eq!(body["timestamp"], "100");
        assert_eq!(body["name"], "thinking_face");
    }
    assert_eq!(adapter.message_limit(), 11900);
    assert!(adapter.renders_native_tables());
    Ok(())
}
#[tokio::test]
async fn api_errors_do_not_retry_unrelated_failures_and_identity_uses_bot_token()
-> Result<(), Box<dyn std::error::Error>> {
    let (url, server) = server(vec![
        (
            json!({"ok":false,"error":"channel_not_found"}).to_string(),
            true,
        ),
        (
            json!({"ok":true,"team_id":"T1","user_id":"UBOT"}).to_string(),
            true,
        ),
    ])
    .await?;
    let adapter = adapter(url)?;
    assert!(
        matches!(adapter.message("chat.postMessage",message_body("C1","text")).await,Err(Error::Api{code,..}) if code=="channel_not_found")
    );
    let identity = adapter.identity().await?;
    assert_eq!(identity.bot, "UBOT");
    assert_eq!(identity.team, "T1");
    let requests = tokio::time::timeout(Duration::from_secs(5), server).await???;
    assert_eq!(requests.len(), 2);
    assert!(requests[1].0.contains("/auth.test"));
    Ok(())
}
#[tokio::test]
async fn private_downloads_enforce_header_and_stream_limits()
-> Result<(), Box<dyn std::error::Error>> {
    for length in [false, true] {
        let (url, server) = server(vec![("12345".into(), length)]).await?;
        let adapter = adapter(url.clone())?;
        assert!(matches!(
            adapter.download(&url, 4).await,
            Err(Error::AttachmentLimit)
        ));
        let requests = tokio::time::timeout(Duration::from_secs(5), server).await???;
        assert_eq!(requests[0].2, "Bearer offline-bot-token");
    }
    Ok(())
}

struct Socket {
    frames: VecDeque<Frame>,
    sent: Vec<Frame>,
}
impl WebSocketConnection for Socket {
    fn send(&mut self, frame: Frame) -> WasmBoxedFuture<'_, rig_http::http_client::Result<()>> {
        Box::pin(async move {
            self.sent.push(frame);
            Ok(())
        })
    }
    fn recv(&mut self) -> WasmBoxedFuture<'_, rig_http::http_client::Result<Option<Frame>>> {
        Box::pin(async move { Ok(self.frames.pop_front()) })
    }
    fn close(
        &mut self,
        _: Option<rig_http::ws_client::CloseFrame>,
    ) -> WasmBoxedFuture<'_, rig_http::http_client::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}
fn handler(
    adapter: SlackAdapter,
    gate: rig_messaging::Gate,
) -> (Handler, rig_core::test_utils::MockCompletionModel) {
    let model = rig_core::test_utils::MockCompletionModel::text("answer");
    let agent = rig_agent::AgentBuilder::new(model.clone())
        .memory(rig_core::memory::InMemoryConversationMemory::new())
        .build();
    let mut cfg = rig_messaging::ChatConfig::default();
    cfg.reactions.enabled = false;
    (
        Handler::new(
            Arc::new(adapter),
            Arc::new(ChatRouter::new(agent, gate, cfg)),
            identity(),
        ),
        model,
    )
}
#[tokio::test]
async fn socket_acknowledges_all_envelopes_and_deduplicates_without_waiting_for_turns()
-> Result<(), Box<dyn std::error::Error>> {
    // The HTTP endpoint never replies. Socket acknowledgements must still finish.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let (handler, _) = handler(
        adapter(format!("http://{}", listener.local_addr()?))?,
        Default::default(),
    );
    let envelope =
        json!({"type":"events_api","envelope_id":"e1","payload":payload("C1","100",None)})
            .to_string();
    let mut socket = Socket {
        frames: vec![
            Frame::Text(envelope.clone()),
            Frame::Text(envelope),
            Frame::Ping(Default::default()),
            Frame::Text(json!({"type":"interactive","envelope_id":"e2"}).to_string()),
            Frame::Close(None),
        ]
        .into(),
        sent: vec![],
    };
    let mut recent = Recent::default();
    tokio::time::timeout(
        Duration::from_secs(1),
        handler.socket(&mut socket, &mut recent),
    )
    .await??;
    assert_eq!(recent.entries.len(), 1);
    assert_eq!(socket.sent.len(), 4);
    assert!(matches!(&socket.sent[2], Frame::Pong(_)));
    for index in [0, 1, 3] {
        let Frame::Text(text) = &socket.sent[index] else {
            return Err(Error::Missing("ack").into());
        };
        let ack: Value = serde_json::from_str(text)?;
        assert!(ack.get("envelope_id").is_some());
    }
    Ok(())
}
#[tokio::test]
async fn rejected_input_does_not_download_or_call_model() -> Result<(), Box<dyn std::error::Error>>
{
    let (handler, model) = handler(
        adapter("http://127.0.0.1:1".into())?,
        rig_messaging::Gate {
            allowed_users: Some(HashSet::from(["other".into()])),
            ..Default::default()
        },
    );
    let mut event = payload("C1", "100", None);
    event["event"]["files"] =
        json!([{"name":"secret","url_private":"https://files.slack.com/file","size":4}]);
    handler.process(event).await?;
    assert_eq!(model.request_count(), 0);
    Ok(())
}

#[tokio::test]
async fn two_slack_turns_share_history_and_preserve_native_tables()
-> Result<(), Box<dyn std::error::Error>> {
    use rig_core::{
        message::Message,
        test_utils::{MockCompletionModel, MockStreamEvent},
    };
    let table = "| a | b |\n|---|---|\n| c | d |";
    let turn = |text: &str| {
        vec![
            MockStreamEvent::text(text),
            MockStreamEvent::final_response(Default::default()),
        ]
    };
    let model = MockCompletionModel::from_stream_turns([turn(table), turn("second reply")]);
    let agent = rig_agent::AgentBuilder::new(model.clone())
        .memory(rig_core::memory::InMemoryConversationMemory::new())
        .build();
    let mut cfg = rig_messaging::ChatConfig::default();
    cfg.reactions.enabled = false;
    let (url, server) = server(
        (0..4)
            .map(|_| (json!({"ok":true,"ts":"200"}).to_string(), true))
            .collect(),
    )
    .await?;
    let handler = Handler::new(
        Arc::new(adapter(url)?),
        Arc::new(ChatRouter::new(agent, Default::default(), cfg)),
        identity(),
    );
    handler.process(payload("C1", "100", None)).await?;
    handler.process(payload("C1", "101", Some("100"))).await?;
    let requests = tokio::time::timeout(Duration::from_secs(5), server).await???;
    let final_body: Value = serde_json::from_str(&requests[1].1)?;
    assert_eq!(final_body["blocks"][0]["text"], table);
    assert_eq!(requests.len(), 4);
    assert!(
        model.requests()[1]
            .chat_history
            .contains(&Message::assistant(table))
    );
    assert!(
        model.requests()[1]
            .chat_history
            .contains(&Message::user("[U1 (U1)]\nhello"))
    );
    Ok(())
}
#[tokio::test]
async fn slack_long_reply_splits_at_unicode_limit_through_router()
-> Result<(), Box<dyn std::error::Error>> {
    let text = "界".repeat(LIMIT + 1);
    let model = rig_core::test_utils::MockCompletionModel::from_stream_turns([vec![
        rig_core::test_utils::MockStreamEvent::text(&text),
        rig_core::test_utils::MockStreamEvent::final_response(Default::default()),
    ]]);
    let agent = rig_agent::AgentBuilder::new(model)
        .memory(rig_core::memory::InMemoryConversationMemory::new())
        .build();
    let mut cfg = rig_messaging::ChatConfig::default();
    cfg.reactions.enabled = false;
    let (url, server) = server(
        (0..3)
            .map(|_| (json!({"ok":true,"ts":"200"}).to_string(), true))
            .collect(),
    )
    .await?;
    let handler = Handler::new(
        Arc::new(adapter(url)?),
        Arc::new(ChatRouter::new(agent, Default::default(), cfg)),
        identity(),
    );
    handler.process(payload("C1", "100", None)).await?;
    let requests = tokio::time::timeout(Duration::from_secs(5), server).await???;
    assert_eq!(requests.len(), 3);
    let mut rendered = String::new();
    for (_, body, _) in &requests[1..] {
        let body: Value = serde_json::from_str(body)?;
        let chunk = body["blocks"][0]["text"]
            .as_str()
            .ok_or(Error::Missing("text"))?;
        assert!(chunk.chars().count() <= LIMIT);
        rendered.push_str(chunk);
    }
    assert_eq!(rendered, text);
    Ok(())
}
#[tokio::test]
async fn real_websocket_receives_envelopes_and_returns_ack()
-> Result<(), Box<dyn std::error::Error>> {
    use futures::{SinkExt, StreamExt};
    use rig_tungstenite::tokio_tungstenite::{accept_async, tungstenite::Message};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await.map_err(std::io::Error::other)?;
        socket
            .send(Message::Text(
                json!({"type":"interactive","envelope_id":"socket-ack"})
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(std::io::Error::other)?;
        let response = socket
            .next()
            .await
            .ok_or_else(|| std::io::Error::other("missing ack"))?
            .map_err(std::io::Error::other)?;
        socket
            .send(Message::Text(
                json!({"type":"disconnect"}).to_string().into(),
            ))
            .await
            .map_err(std::io::Error::other)?;
        Ok::<_, std::io::Error>(response)
    });
    let request = Request::builder().uri(address).body(NoBody)?;
    let mut socket = rig_tungstenite::TungsteniteClient::new()
        .connect(
            request,
            ConnectOptions::new().with_timeout(Some(Duration::from_secs(2))),
        )
        .await?;
    let (handler, _) = handler(adapter("http://127.0.0.1:1".into())?, Default::default());
    tokio::time::timeout(
        Duration::from_secs(5),
        handler.socket(socket.as_mut(), &mut Recent::default()),
    )
    .await??;
    let response = tokio::time::timeout(Duration::from_secs(5), server).await???;
    let ack: Value = serde_json::from_str(response.to_text()?)?;
    assert_eq!(ack["envelope_id"], "socket-ack");
    Ok(())
}

struct IdleSocket {
    sent: Vec<Frame>,
}
impl WebSocketConnection for IdleSocket {
    fn send(&mut self, frame: Frame) -> WasmBoxedFuture<'_, rig_http::http_client::Result<()>> {
        Box::pin(async move {
            self.sent.push(frame);
            Ok(())
        })
    }
    fn recv(&mut self) -> WasmBoxedFuture<'_, rig_http::http_client::Result<Option<Frame>>> {
        Box::pin(std::future::pending())
    }
    fn close(
        &mut self,
        _: Option<rig_http::ws_client::CloseFrame>,
    ) -> WasmBoxedFuture<'_, rig_http::http_client::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}
#[tokio::test(start_paused = true)]
async fn idle_socket_pings_then_times_out_if_peer_never_answers() -> Result<(), Error> {
    let (handler, _) = handler(adapter("http://127.0.0.1:1".into())?, Default::default());
    let mut socket = IdleSocket { sent: vec![] };
    let result = handler.socket(&mut socket, &mut Recent::default()).await;
    assert!(matches!(result, Err(Error::Timeout(_))));
    assert_eq!(socket.sent, vec![Frame::Ping(Default::default())]);
    Ok(())
}
#[tokio::test]
async fn socket_url_uses_app_token_and_empty_form_body() -> Result<(), Box<dyn std::error::Error>> {
    let (url, server) = server(vec![(
        json!({"ok":true,"url":"wss://example.invalid/socket"}).to_string(),
        true,
    )])
    .await?;
    let adapter = adapter(url)?;
    let response = adapter
        .api("apps.connections.open", &json!({}), "offline-app-token")
        .await?;
    assert_eq!(field(&response, "url")?, "wss://example.invalid/socket");
    let requests = tokio::time::timeout(Duration::from_secs(5), server).await???;
    assert!(requests[0].0.contains("/apps.connections.open"));
    assert_eq!(requests[0].1, "");
    assert_eq!(requests[0].2, "Bearer offline-app-token");
    Ok(())
}
