#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
use rig_core::{
    message::Message,
    test_utils::{MockCompletionModel, MockStreamEvent},
};

#[tokio::test]
async fn piped_lines_preserve_order_and_history_without_edits_or_reactions()
-> Result<(), Box<dyn std::error::Error>> {
    let turn = |text: &str| {
        vec![
            MockStreamEvent::text(text),
            MockStreamEvent::final_response(Default::default()),
        ]
    };
    let model = MockCompletionModel::from_stream_turns([turn("reply one"), turn("reply two")]);
    let agent = AgentBuilder::new(model.clone())
        .memory(InMemoryConversationMemory::new())
        .build();
    let router = ChatRouter::new(agent, Gate::default(), ChatConfig::default());
    let adapter = Arc::new(StdioAdapter::new(Vec::<u8>::new()));
    run_lines(
        BufReader::new(&b"first\nsecond\n"[..]),
        &router,
        adapter.clone(),
        "user".into(),
    )
    .await?;
    let writer = adapter
        .writer
        .lock()
        .map_err(|_| std::io::Error::other("lock poisoned"))?;
    assert_eq!(std::str::from_utf8(&writer)?, "reply one\nreply two\n");
    assert_eq!(adapter.next_message.load(Ordering::SeqCst), 2);
    assert_eq!(adapter.unexpected_calls.load(Ordering::SeqCst), 0);
    assert!(
        model.requests()[1]
            .chat_history
            .contains(&Message::assistant("reply one"))
    );
    assert!(
        model.requests()[1]
            .chat_history
            .contains(&Message::user("[user (local)]\nfirst"))
    );
    assert_eq!(model.request_count(), 2);
    Ok(())
}
