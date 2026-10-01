//! Credentialed tests create messages only in SLACK_TEST_CHANNEL.

use super::*;
use rig_messaging::{ChatConfig, Gate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub(super) fn check(condition: bool, message: &'static str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(std::io::Error::other(message).into())
    }
}

pub(super) async fn setup() -> TestResult<(Arc<SlackAdapter>, Identity, ChannelRef)> {
    dotenvy::dotenv()?;
    let channel = std::env::var("SLACK_TEST_CHANNEL")?;
    check(
        crate::allowlist("SLACK_ALLOWED_CHANNELS")
            .is_some_and(|allowed| allowed.contains(&channel)),
        "SLACK_TEST_CHANNEL must appear in SLACK_ALLOWED_CHANNELS",
    )?;
    let adapter = Arc::new(SlackAdapter::new(std::env::var("SLACK_BOT_TOKEN")?)?);
    let identity = adapter.identity().await?;
    let channel = ChannelRef {
        platform: "slack".into(),
        scope_id: Some(identity.team.clone()),
        channel_id: channel,
        thread_id: None,
    };
    Ok((adapter, identity, channel))
}

async fn root_message(adapter: &SlackAdapter, message: &MessageRef) -> TestResult<Value> {
    let result = adapter
        .api(
            "conversations.history",
            &json!({
                "channel":message.channel.channel_id,"oldest":message.message_id,
                "latest":message.message_id,"inclusive":true,"limit":1
            }),
            &adapter.token,
        )
        .await?;
    Ok(result
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .find(|item| item.get("ts").and_then(Value::as_str) == Some(&message.message_id))
        })
        .cloned()
        .ok_or(Error::Missing("test message in history"))?)
}

pub(super) async fn replies(adapter: &SlackAdapter, root: &MessageRef) -> TestResult<Vec<Value>> {
    let result = adapter
        .api(
            "conversations.replies",
            &json!({
                "channel":root.channel.channel_id,"ts":root.message_id,"limit":15
            }),
            &adapter.token,
        )
        .await?;
    Ok(result
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or(Error::Missing("thread messages"))?)
}

#[tokio::test]
#[ignore = "sends, reads, edits and deletes real Slack messages"]
async fn live_slack_message_lifecycle() -> TestResult {
    let (adapter, _, channel) = setup().await?;
    let root = adapter
        .send(&channel, "Rig live test: message lifecycle")
        .await?;
    let mut child = None;
    let result: TestResult = async {
        check(
            root_message(&adapter, &root)
                .await?
                .get("text")
                .and_then(Value::as_str)
                == Some("Rig live test: message lifecycle"),
            "sent text was not readable",
        )?;
        adapter
            .edit(&root, "Rig live test: edited successfully")
            .await?;
        check(
            root_message(&adapter, &root)
                .await?
                .get("text")
                .and_then(Value::as_str)
                == Some("Rig live test: edited successfully"),
            "edited text did not match",
        )?;
        adapter.add_reaction(&root, "✅").await?;
        let reaction = adapter
            .api(
                "reactions.get",
                &json!({"channel":channel.channel_id,"timestamp":root.message_id}),
                &adapter.token,
            )
            .await?;
        check(
            reaction
                .get("message")
                .and_then(|message| message.get("reactions"))
                .and_then(Value::as_array)
                .is_some_and(|reactions| {
                    reactions.iter().any(|reaction| {
                        reaction.get("name").and_then(Value::as_str) == Some("white_check_mark")
                    })
                }),
            "reaction was not readable",
        )?;
        adapter.remove_reaction(&root, "✅").await?;
        check(
            root_message(&adapter, &root)
                .await?
                .get("reactions")
                .and_then(Value::as_array)
                .is_none_or(|reactions| {
                    !reactions.iter().any(|reaction| {
                        reaction.get("name").and_then(Value::as_str) == Some("white_check_mark")
                    })
                }),
            "removed reaction remained visible",
        )?;
        let mut thread = channel.clone();
        thread.thread_id = Some(root.message_id.clone());
        child = Some(
            adapter
                .send(&thread, "Rig live test: threaded reply")
                .await?,
        );
        check(
            replies(&adapter, &root).await?.iter().any(|item| {
                item.get("text").and_then(Value::as_str) == Some("Rig live test: threaded reply")
            }),
            "thread reply was not readable",
        )?;
        Ok(())
    }
    .await;
    let child_cleanup = match child {
        Some(child) => adapter.delete(&child).await,
        None => Ok(()),
    };
    let root_cleanup = adapter.delete(&root).await;
    result?;
    child_cleanup?;
    root_cleanup?;
    let deleted = root_message(&adapter, &root).await;
    check(
        matches!(
            deleted
                .as_ref()
                .err()
                .and_then(|error| error.downcast_ref::<Error>()),
            Some(Error::Missing("test message in history"))
        ),
        "deleted message remained visible or could not be checked",
    )?;
    eprintln!("Slack live lifecycle passed: send/read/edit/reactions/thread/delete");
    Ok(())
}

#[tokio::test]
#[ignore = "uses real Go quota and leaves a model conversation in the test Slack channel"]
async fn live_slack_model_router() -> TestResult {
    let (adapter, identity, channel) = setup().await?;
    let (agent, go) = crate::provider::agent_from_env().await?;
    check(go, "model router validation requires Go credentials")?;
    let root = adapter
        .send(&channel, "Rig live model test: two Rust assistance turns")
        .await?;
    let gate = Gate {
        allowed_channels: Some([channel.channel_id.clone()].into_iter().collect()),
        ..Default::default()
    };
    let handler = Handler::new(
        adapter.clone(),
        Arc::new(ChatRouter::new(agent, gate, ChatConfig::default())),
        identity.clone(),
    );
    for (index, prompt) in [
        "Explain Rust Iterator::map in at most 30 words.",
        "Give one short Rust example for the method from our previous turn.",
    ]
    .into_iter()
    .enumerate()
    {
        let timestamp = if index == 0 {
            root.message_id.clone()
        } else {
            let mut thread = channel.clone();
            thread.thread_id = Some(root.message_id.clone());
            adapter.send(&thread, prompt).await?.message_id
        };
        // The bot's own events are intentionally rejected. Inject a test sender
        // to exercise ingress and the router without impersonating a Slack user.
        handler.process(json!({"team_id":identity.team,"event":{
            "type":"app_mention","channel":channel.channel_id,"user":"rig-live-test",
            "text":format!("<@{}> {prompt}",identity.bot),"ts":timestamp,"thread_ts":root.message_id
        }})).await?;
    }
    let messages = replies(&adapter, &root).await?;
    check(
        messages.len() >= 4,
        "two model replies were not readable in the thread",
    )?;
    check(
        !messages.iter().any(|message| {
            message
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains("⚠") || text == "…")
        }),
        "model turn left a warning or unfinished preview",
    )?;
    let reaction = adapter
        .api(
            "reactions.get",
            &json!({"channel":channel.channel_id,"timestamp":root.message_id}),
            &adapter.token,
        )
        .await?;
    check(
        reaction
            .get("message")
            .and_then(|message| message.get("reactions"))
            .and_then(Value::as_array)
            .is_some_and(|reactions| {
                reactions
                    .iter()
                    .any(|reaction| reaction.get("name").and_then(Value::as_str) == Some("ok"))
            }),
        "router did not leave its done reaction",
    )?;
    eprintln!(
        "Slack live model conversation: https://app.slack.com/client/{}/{}/thread/{}-{}",
        identity.team, channel.channel_id, channel.channel_id, root.message_id
    );
    Ok(())
}

#[tokio::test]
#[ignore = "connects real Socket Mode and posts a test message"]
async fn live_slack_socket_receives_and_acknowledges_message() -> TestResult {
    let (adapter, _, channel) = setup().await?;
    let app = std::env::var("SLACK_APP_TOKEN")?;
    let opened = adapter
        .api("apps.connections.open", &json!({}), &app)
        .await?;
    let request = Request::builder()
        .uri(field(&opened, "url")?)
        .body(NoBody)?;
    let mut socket = rig_tungstenite::TungsteniteClient::new()
        .connect(
            request,
            ConnectOptions::new().with_timeout(Some(Duration::from_secs(15))),
        )
        .await?;
    let root = adapter
        .send(&channel, "Rig live test: Socket Mode delivery")
        .await?;
    let result: TestResult = async {
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                match socket.recv().await? {
                    Some(Frame::Text(text)) => {
                        let envelope: Value = serde_json::from_str(&text)?;
                        if let Some(id) = envelope.get("envelope_id").and_then(Value::as_str) {
                            socket
                                .send(Frame::Text(json!({"envelope_id":id}).to_string()))
                                .await?;
                        }
                        let event = envelope
                            .get("payload")
                            .and_then(|payload| payload.get("event"));
                        if event
                            .and_then(|event| event.get("ts"))
                            .and_then(Value::as_str)
                            == Some(root.message_id.as_str())
                        {
                            return Ok::<_, Box<dyn std::error::Error>>(());
                        }
                    }
                    Some(Frame::Ping(data)) => socket.send(Frame::Pong(data)).await?,
                    Some(Frame::Close(_)) | None => {
                        return Err(
                            std::io::Error::other("Slack socket closed before test event").into(),
                        );
                    }
                    _ => {}
                }
            }
        })
        .await??;
        Ok(())
    }
    .await;
    let cleanup = adapter.delete(&root).await;
    result?;
    cleanup?;
    eprintln!("Slack live Socket Mode passed: real message event received and acknowledged");
    Ok(())
}
