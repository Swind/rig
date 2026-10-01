use super::{
    live_tests::{check, replies, setup},
    *,
};
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

#[tokio::test]
#[ignore = "posts and deletes a real Slack table and Unicode reply exceeding the message limit"]
async fn live_slack_table_and_long_unicode_reply() -> Result<(), Box<dyn std::error::Error>> {
    let (adapter, identity, channel) = setup().await?;
    let table = "| Rust method | Result |\n|---|---|\n| map | transformed iterator |";
    let text = format!("{table}\n\n{}", "界".repeat(LIMIT + 10));
    let model = MockCompletionModel::from_stream_turns([vec![
        MockStreamEvent::text(&text),
        MockStreamEvent::final_response(Default::default()),
    ]]);
    let agent = rig_agent::AgentBuilder::new(model)
        .memory(rig_core::memory::InMemoryConversationMemory::new())
        .build();
    let mut cfg = rig_messaging::ChatConfig::default();
    cfg.reactions.enabled = false;
    let handler = Handler::new(
        adapter.clone(),
        Arc::new(ChatRouter::new(agent, Default::default(), cfg)),
        identity.clone(),
    );
    let root = adapter
        .send(&channel, "Rig live test: table and long Unicode delivery")
        .await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        handler.process(json!({"team_id":identity.team,"event":{
            "type":"message","channel":channel.channel_id,"user":"rig-live-test",
            "text":"Return the test table and Unicode body.","ts":root.message_id,"thread_ts":root.message_id
        }})).await?;
        let messages = replies(&adapter, &root).await?;
        let messages: Vec<_> = messages.iter().filter(|message| message.get("ts").and_then(Value::as_str) != Some(root.message_id.as_str())).collect();
        check(messages.len() >= 2, "long reply was not split")?;
        let mut unicode = 0;
        let mut native_table = false;
        for message in messages {
            let text = field(message, "text")?;
            check(text.chars().count() <= LIMIT, "reply exceeded Slack character limit")?;
            unicode += text.chars().filter(|character| *character == '界').count();
            native_table |= message.get("blocks").and_then(Value::as_array).is_some_and(|blocks| blocks.iter().any(|block|
                match block.get("type").and_then(Value::as_str) {
                    Some("markdown") => block.get("text").and_then(Value::as_str).is_some_and(|text| text.contains(table)),
                    Some("table") => block.get("rows").is_some_and(|rows| {
                        let rows = rows.to_string();
                        ["Rust method", "Result", "map", "transformed iterator"].iter().all(|cell| rows.contains(cell))
                    }),
                    _ => false,
                }));
        }
        check(unicode == LIMIT + 10, "Unicode reply was truncated or duplicated")?;
        check(native_table, "table was missing from Slack rendered blocks")?;
        Ok(())
    }.await;
    let cleanup: Result<(), Box<dyn std::error::Error>> = async {
        for message in replies(&adapter, &root).await?.into_iter().rev() {
            if message.get("user").and_then(Value::as_str) == Some(identity.bot.as_str()) {
                adapter
                    .delete(&MessageRef {
                        channel: channel.clone(),
                        message_id: field(&message, "ts")?.into(),
                    })
                    .await?;
            }
        }
        Ok(())
    }
    .await;
    result?;
    cleanup?;
    eprintln!("Slack live layout passed: native table and lossless Unicode splitting");
    Ok(())
}
