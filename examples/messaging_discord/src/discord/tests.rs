use super::*;

fn fixture(channel_type: u8, thread: bool) -> Result<(Message, Channel), serde_json::Error> {
    let msg = serde_json::from_value(
        serde_json::json!({"id":"100","channel_id":"200","guild_id":"300","author":{"id":"400","username":"alice","discriminator":"0001","avatar":null},"content":"<@500> hello","timestamp":"2026-09-30T00:00:00Z","edited_timestamp":null,"tts":false,"mention_everyone":false,"mentions":[{"id":"500","username":"bot","discriminator":"0001","avatar":null,"bot":true}],"mention_roles":[],"attachments":[],"embeds":[],"pinned":false,"type":0}),
    )?;
    let mut ch = serde_json::json!({"id":"200","guild_id":"300","type":channel_type,"name":"room","parent_id":"600"});
    if thread {
        ch["thread_metadata"] = serde_json::json!({"archived":false,"auto_archive_duration":1440,"archive_timestamp":"2026-09-30T00:00:00Z","locked":false});
    }
    Ok((msg, serde_json::from_value(ch)?))
}

#[test]
fn parent_category_is_not_a_thread() -> Result<(), Box<dyn std::error::Error>> {
    let (msg, ch) = fixture(0, false)?;
    let input = normalize(&msg, &ch, UserId::new(500));
    assert!(!input.is_thread);
    assert!(input.mentions_bot);
    assert_eq!(input.context.channel_name.as_deref(), Some("room"));
    assert_eq!(
        input.context.sent_at.map(|time| time.to_rfc3339()),
        Some("2026-09-30T00:00:00+00:00".into())
    );
    assert_eq!(
        input
            .context
            .mentions
            .first()
            .map(|sender| sender.id.as_str()),
        Some("500")
    );
    assert_eq!(
        input.context.mentions.first().map(|sender| sender.is_bot),
        Some(true)
    );
    assert_eq!(input.text, "hello");
    assert!(input.message.channel.thread_id.is_none());
    assert!(rig::messaging::Gate::default().allows(&input, "500"));
    Ok(())
}
#[test]
fn reply_thread_keeps_original_reaction_target() -> Result<(), Box<dyn std::error::Error>> {
    let (msg, ch) = fixture(0, false)?;
    let mut input = normalize(&msg, &ch, UserId::new(500));
    let original = input.message.clone();
    let (_, thread) = fixture(11, true)?;
    if let Channel::Guild(mut thread) = thread {
        thread.id = ChannelId::new(700);
        use_thread(&mut input, &thread);
    }
    assert_eq!(input.message, original);
    assert_eq!(input.reply_channel.channel_id, "700");
    assert!(!input.is_thread);
    Ok(())
}
#[test]
fn existing_thread_does_not_require_mention() -> Result<(), Box<dyn std::error::Error>> {
    let (mut msg, ch) = fixture(11, true)?;
    msg.mentions.clear();
    msg.content = "follow-up".into();
    let input = normalize(&msg, &ch, UserId::new(500));
    assert!(input.is_thread);
    assert!(!input.mentions_bot);
    assert!(rig::messaging::Gate::default().allows(&input, "500"));
    Ok(())
}
#[test]
fn attachment_and_id_limits_reject_invalid_values() {
    assert!(attachment_fits(ATTACHMENT_LIMIT, 0));
    assert!(!attachment_fits(1, ATTACHMENT_LIMIT));
    assert!(!attachment_fits(ATTACHMENT_LIMIT + 1, 0));
    let ch = ChannelRef {
        platform: "discord".into(),
        scope_id: None,
        channel_id: "0".into(),
        thread_id: None,
    };
    assert!(channel_id(&ch).is_err());
    assert!(
        channel_id(&ChannelRef {
            channel_id: "bad".into(),
            ..ch
        })
        .is_err()
    );
}

type Requests = Vec<(String, String)>;
type ServerHandle = std::thread::JoinHandle<std::io::Result<Requests>>;

fn server(responses: Vec<(u16, String, bool)>) -> std::io::Result<(String, ServerHandle)> {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}", listener.local_addr()?);
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body, content_length) in responses {
            let (mut socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
            let mut reader = std::io::BufReader::new(socket.try_clone()?);
            let mut first = String::new();
            reader.read_line(&mut first)?;
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header)?;
                if header == "\r\n" || header.is_empty() {
                    break;
                }
                if let Some(size) = header.to_lowercase().strip_prefix("content-length:") {
                    length = size
                        .trim()
                        .parse::<usize>()
                        .map_err(std::io::Error::other)?;
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes)?;
            requests.push((
                first,
                String::from_utf8(bytes).map_err(std::io::Error::other)?,
            ));
            write!(
                socket,
                "HTTP/1.1 {status} OK\r\nConnection: close\r\nContent-Type: application/json\r\n"
            )?;
            if content_length {
                write!(socket, "Content-Length: {}\r\n", body.len())?;
            }
            write!(socket, "\r\n{body}")?;
        }
        Ok(requests)
    });
    Ok((url, handle))
}
fn join(
    handle: std::thread::JoinHandle<std::io::Result<Vec<(String, String)>>>,
) -> std::io::Result<Vec<(String, String)>> {
    handle
        .join()
        .map_err(|_| std::io::Error::other("fixture server panicked"))?
}
fn http(proxy: &str) -> Arc<Http> {
    Arc::new(
        serenity::all::HttpBuilder::new("offline-fixture")
            .proxy(proxy)
            .ratelimiter_disabled(true)
            .build(),
    )
}

#[tokio::test]
async fn outbound_operations_use_serenity_http_and_original_addresses()
-> Result<(), Box<dyn std::error::Error>> {
    let (msg, _) = fixture(0, false)?;
    let response = serde_json::to_string(&msg)?;
    let (url, handle) = server(vec![
        (200, response.clone(), true),
        (200, response, true),
        (204, String::new(), true),
        (204, String::new(), true),
        (204, String::new(), true),
    ])?;
    let adapter = DiscordAdapter { http: http(&url) };
    let (msg, ch) = fixture(0, false)?;
    let input = normalize(&msg, &ch, UserId::new(500));
    let sent = adapter.send(&input.reply_channel, "reply").await?;
    adapter.edit(&sent, "updated").await?;
    adapter.delete(&sent).await?;
    adapter.add_reaction(&input.message, "🤔").await?;
    adapter.remove_reaction(&input.message, "🤔").await?;
    let requests = join(handle)?;
    assert_eq!(requests.len(), 5);
    assert!(
        requests[0]
            .0
            .contains("POST /api/v10/channels/200/messages ")
    );
    let body: serde_json::Value = serde_json::from_str(&requests[0].1)?;
    assert_eq!(body["content"], "reply");
    assert_eq!(body["allowed_mentions"]["parse"], serde_json::json!([]));
    assert!(
        requests[1]
            .0
            .contains("PATCH /api/v10/channels/200/messages/100 ")
    );
    assert!(
        requests[2]
            .0
            .contains("DELETE /api/v10/channels/200/messages/100 ")
    );
    assert!(
        requests[3].0.starts_with("PUT ")
            && requests[3]
                .0
                .contains("/channels/200/messages/100/reactions/")
    );
    assert!(
        requests[4].0.starts_with("DELETE ")
            && requests[4]
                .0
                .contains("/channels/200/messages/100/reactions/")
    );
    Ok(())
}

#[tokio::test]
async fn bounded_download_checks_headers_and_streamed_bytes()
-> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    for has_length in [false, true] {
        let (url, handle) = server(vec![(200, "12345".into(), has_length)])?;
        assert!(download(&client, &url, 4).await.is_err());
        join(handle)?;
    }
    let (url, handle) = server(vec![(200, "1234".into(), false)])?;
    assert_eq!(download(&client, &url, 4).await?, b"1234");
    join(handle)?;
    Ok(())
}

#[tokio::test]
async fn rejected_input_creates_no_thread_and_downloads_no_attachments()
-> Result<(), Box<dyn std::error::Error>> {
    let (msg, ch) = fixture(0, false)?;
    let (url, handle) = server(vec![(200, serde_json::to_string(&ch)?, true)])?;
    let mut value = serde_json::to_value(msg)?;
    value["attachments"] = serde_json::json!([{"id":"900","filename":"image.png","size":4,"url":url,"proxy_url":url,"content_type":"image/png"}]);
    let msg = serde_json::from_value(value)?;
    let model = rig::test_utils::MockCompletionModel::text("unused");
    let agent = rig::AgentBuilder::new(model.clone())
        .memory(rig::memory::InMemoryConversationMemory::new())
        .build();
    let gate = rig::messaging::Gate {
        allowed_users: Some(std::collections::HashSet::from(["other".into()])),
        ..Default::default()
    };
    let router = Arc::new(ChatRouter::new(agent, gate, Default::default()));
    let handler = Handler::new(router, UserId::new(500))?;
    handler.process(http(&url), msg).await?;
    let requests = join(handle)?;
    assert_eq!(requests.len(), 1);
    assert!(requests[0].0.starts_with("GET /api/v10/channels/200 "));
    assert_eq!(model.request_count(), 0);
    Ok(())
}
