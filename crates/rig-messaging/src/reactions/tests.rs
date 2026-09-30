#![allow(clippy::panic_in_result_fn)]
use super::*;
use crate::{
    ChatConfig, ChatRouter, Gate,
    test_support::{Call, FakeAdapter, inbound, text_turn},
};
use futures::StreamExt;
use rig_agent::AgentBuilder;
use rig_core::{
    memory::InMemoryConversationMemory,
    test_utils::{MockCompletionModel, MockStreamEvent},
};

async fn flush() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}
async fn advance(ms: u64) {
    flush().await;
    tokio::time::advance(Duration::from_millis(ms)).await;
    flush().await;
}
fn fake_adapter() -> Arc<FakeAdapter> {
    Arc::new(FakeAdapter {
        reactions: true,
        ..Default::default()
    })
}
fn added(adapter: &FakeAdapter) -> Vec<String> {
    adapter
        .calls()
        .into_iter()
        .filter_map(|call| {
            if let Call::Add(_, s) = call {
                Some(s)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn defaults_and_tool_classification_match_contract() {
    let cfg = ReactionConfig::default();
    assert!(cfg.enabled);
    assert!(!cfg.remove_after_reply);
    assert_eq!(
        (
            cfg.timing.debounce_ms,
            cfg.timing.stall_soft_ms,
            cfg.timing.stall_hard_ms,
            cfg.timing.done_hold_ms,
            cfg.timing.error_hold_ms
        ),
        (700, 10000, 30000, 1500, 2500)
    );
    for tool in ["EXEC", "process", "read", "write", "edit", "bash", "shell"] {
        assert_eq!(classify_tool(tool, &cfg.emojis), "👨‍💻");
    }
    for tool in [
        "web_search",
        "web_fetch",
        "web-search",
        "web-fetch",
        "browser_read",
    ] {
        assert_eq!(classify_tool(tool, &cfg.emojis), "⚡");
    }
    assert_eq!(classify_tool("add", &cfg.emojis), "🔥");
}

#[tokio::test(start_paused = true)]
async fn progress_adds_before_removing_and_terminal_state_ignores_updates() {
    let adapter = fake_adapter();
    let message = inbound("first").message;
    let ctl = StatusReactions::new(adapter.clone(), message.clone(), Default::default());
    ctl.set_queued().await;
    ctl.set_thinking();
    advance(700).await;
    ctl.set_tool("exec");
    advance(700).await;
    ctl.set_thinking();
    advance(700).await;
    ctl.set_done().await;
    let calls = adapter.calls();
    let states = added(&adapter);
    assert_eq!(&states[..5], ["👀", "🤔", "👨‍💻", "🤔", "🆗"]);
    assert_eq!(states.len(), 6);
    assert_eq!(
        &calls[1..3],
        [
            Call::Add(message.clone(), "🤔".into()),
            Call::Remove(message.clone(), "👀".into())
        ]
    );
    let count = calls.len();
    ctl.set_tool("browser");
    ctl.set_thinking();
    ctl.touch();
    advance(40000).await;
    assert_eq!(adapter.calls().len(), count);
    ctl.clear().await;
    let calls = adapter.calls();
    assert!(matches!(&calls[calls.len()-2],Call::Remove(_,s) if s=="🆗"));
    assert!(matches!(&calls[calls.len()-1],Call::Remove(_,s) if s==&states[5]));
}

#[tokio::test(start_paused = true)]
async fn last_debounced_switch_wins_including_return_to_current_state() {
    let adapter = fake_adapter();
    let ctl = StatusReactions::new(
        adapter.clone(),
        inbound("first").message,
        Default::default(),
    );
    ctl.set_queued().await;
    ctl.set_thinking();
    advance(200).await;
    ctl.set_tool("exec");
    advance(200).await;
    ctl.set_tool("browser");
    advance(700).await;
    assert_eq!(added(&adapter), ["👀", "⚡"]);
    ctl.set_thinking();
    advance(200).await;
    ctl.set_tool("browser");
    advance(700).await;
    assert_eq!(added(&adapter), ["👀", "⚡"]);
    ctl.set_error().await;
}

#[tokio::test(start_paused = true)]
async fn stall_thresholds_reset_on_text_progress() {
    let adapter = fake_adapter();
    let ctl = StatusReactions::new(
        adapter.clone(),
        inbound("first").message,
        Default::default(),
    );
    ctl.set_queued().await;
    advance(10000).await;
    assert_eq!(added(&adapter).last().map(String::as_str), Some("🥱"));
    advance(20000).await;
    assert_eq!(added(&adapter).last().map(String::as_str), Some("😨"));
    ctl.set_thinking();
    advance(700).await;
    ctl.touch();
    flush().await;
    let before = adapter.calls().len();
    advance(9999).await;
    assert_eq!(adapter.calls().len(), before);
    advance(1).await;
    assert_eq!(added(&adapter).last().map(String::as_str), Some("🥱"));
    ctl.set_error().await;
}

#[tokio::test(start_paused = true)]
async fn disabled_controller_and_reaction_errors_do_not_affect_replies()
-> Result<(), Box<dyn std::error::Error>> {
    let adapter = Arc::new(FakeAdapter::default());
    let ctl = StatusReactions::new(
        adapter.clone(),
        inbound("first").message,
        Default::default(),
    );
    ctl.set_queued().await;
    ctl.set_thinking();
    ctl.set_tool("exec");
    ctl.touch();
    ctl.set_done().await;
    ctl.clear().await;
    assert!(adapter.calls().is_empty());
    let enabled = fake_adapter();
    enabled.fail_reactions.store(usize::MAX, Ordering::SeqCst);
    let agent = AgentBuilder::new(MockCompletionModel::from_stream_turns([text_turn("reply")]))
        .memory(InMemoryConversationMemory::new())
        .build();
    let router = ChatRouter::new(agent, Gate::default(), ChatConfig::default());
    router
        .handle(enabled.clone(), inbound("first"), "bot")
        .await?;
    assert!(
        enabled
            .calls()
            .iter()
            .any(|call| matches!(call,Call::Edit(_,text) if text=="reply"))
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn delivery_failure_sets_error_on_original_message() {
    let adapter = fake_adapter();
    adapter.fail_sends.store(usize::MAX, Ordering::SeqCst);
    let agent = AgentBuilder::new(MockCompletionModel::from_stream_turns([text_turn("reply")]))
        .memory(InMemoryConversationMemory::new())
        .build();
    let router = ChatRouter::new(agent, Gate::default(), ChatConfig::default());
    let mut message = inbound("first");
    message.reply_channel.channel_id = "new-thread".into();
    assert!(
        router
            .handle(adapter.clone(), message.clone(), "bot")
            .await
            .is_err()
    );
    assert_eq!(added(&adapter), ["👀", "😱"]);
    assert!(
        adapter
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::Add(..) | Call::Remove(..)))
            .all(|call| matches!(call,Call::Add(m,_)|Call::Remove(m,_) if m==&message.message))
    );
}

#[tokio::test(start_paused = true)]
async fn separate_controllers_do_not_share_state() {
    let adapter = fake_adapter();
    let first = StatusReactions::new(
        adapter.clone(),
        inbound("first").message,
        Default::default(),
    );
    let second = StatusReactions::new(
        adapter.clone(),
        inbound("second").message,
        Default::default(),
    );
    first.set_queued().await;
    second.set_queued().await;
    first.set_tool("browser");
    second.set_tool("exec");
    advance(700).await;
    first.set_error().await;
    second.set_done().await;
    let calls = adapter.calls();
    assert!(calls.contains(&Call::Add(inbound("first").message, "⚡".into())));
    assert!(calls.contains(&Call::Add(inbound("second").message, "👨‍💻".into())));
    assert!(!calls.contains(&Call::Add(inbound("first").message, "🆗".into())));
    assert!(!calls.contains(&Call::Add(inbound("second").message, "😱".into())));
}

#[tokio::test(start_paused = true)]
async fn hooks_finish_while_reaction_network_is_blocked() -> Result<(), Box<dyn std::error::Error>>
{
    let release = Arc::new(tokio::sync::Notify::new());
    let adapter = Arc::new(FakeAdapter {
        reactions: true,
        reaction_block: Some(release.clone()),
        ..Default::default()
    });
    let ctl = Arc::new(StatusReactions::new(
        adapter.clone(),
        inbound("first").message,
        ReactionConfig {
            timing: ReactionTiming {
                debounce_ms: 0,
                ..Default::default()
            },
            ..Default::default()
        },
    ));
    ctl.set_thinking();
    flush().await;
    assert_eq!(added(&adapter), ["🤔"]);
    let agent = AgentBuilder::new(MockCompletionModel::from_stream_turns([text_turn("reply")]))
        .memory(InMemoryConversationMemory::new())
        .build();
    let mut stream = agent
        .prompt("first")
        .conversation("thread")
        .add_hook(ReactionHook::new(ctl.clone()))
        .stream();
    let final_reply = tokio::time::timeout(Duration::from_millis(1), async {
        while let Some(item) = stream.next().await {
            if matches!(item?, MultiTurnStreamItem::FinalResponse(_)) {
                return Ok::<_, rig_agent::agent::StreamingError>(true);
            }
        }
        Ok(false)
    })
    .await??;
    assert!(final_reply);
    release.notify_one();
    flush().await;
    ctl.clear().await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn real_tool_dispatch_drives_status_before_tool_completes()
-> Result<(), Box<dyn std::error::Error>> {
    let started = Arc::new(tokio::sync::Notify::new());
    let finish = Arc::new(tokio::sync::Notify::new());
    let model = MockCompletionModel::from_stream_turns([
        vec![
            MockStreamEvent::tool_call("tc1", "controlled", serde_json::json!({})),
            MockStreamEvent::final_response(Default::default()),
        ],
        text_turn("done"),
    ]);
    let agent = AgentBuilder::new(model)
        .tool(rig_agent::test_utils::MockControlledTool::new(
            started.clone(),
            finish.clone(),
        ))
        .default_max_turns(3)
        .memory(InMemoryConversationMemory::new())
        .build();
    let router = Arc::new(ChatRouter::new(
        agent,
        Gate::default(),
        ChatConfig::default(),
    ));
    let adapter = fake_adapter();
    let task = tokio::spawn({
        let router = router.clone();
        let adapter = adapter.clone();
        async move { router.handle(adapter, inbound("first"), "bot").await }
    });
    started.notified().await;
    advance(700).await;
    assert!(added(&adapter).contains(&"🔥".into()));
    finish.notify_one();
    task.await??;
    assert!(added(&adapter).contains(&"🆗".into()));
    Ok(())
}

use rig_agent::agent::MultiTurnStreamItem;

#[tokio::test(start_paused = true)]
async fn text_touch_requires_a_full_second_between_updates() {
    let adapter = fake_adapter();
    let ctl = StatusReactions::new(adapter, inbound("first").message, Default::default());
    ctl.touch();
    assert_eq!(ctl.last_touch.load(Ordering::Relaxed), 0);
    advance(999).await;
    ctl.touch();
    assert_eq!(ctl.last_touch.load(Ordering::Relaxed), 0);
    advance(1).await;
    ctl.touch();
    assert_eq!(ctl.last_touch.load(Ordering::Relaxed), 1000);
    ctl.set_error().await;
}

#[tokio::test(start_paused = true)]
async fn reaction_hold_releases_conversation_lock() -> Result<(), Box<dyn std::error::Error>> {
    let model = MockCompletionModel::from_stream_turns([text_turn("one"), text_turn("two")]);
    let agent = AgentBuilder::new(model.clone())
        .memory(InMemoryConversationMemory::new())
        .build();
    let cfg = ChatConfig {
        reactions: ReactionConfig {
            remove_after_reply: true,
            timing: ReactionTiming {
                done_hold_ms: 10000,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let router = Arc::new(ChatRouter::new(agent, Gate::default(), cfg));
    let adapter = fake_adapter();
    let first = tokio::spawn({
        let router = router.clone();
        let adapter = adapter.clone();
        async move { router.handle(adapter, inbound("first"), "bot").await }
    });
    flush().await;
    assert!(added(&adapter).contains(&"🆗".into()));
    assert!(!first.is_finished());
    let second = tokio::spawn({
        let router = router.clone();
        let adapter = adapter.clone();
        async move { router.handle(adapter, inbound("second"), "bot").await }
    });
    flush().await;
    assert_eq!(model.request_count(), 2);
    advance(10000).await;
    first.await??;
    second.await??;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn concurrent_router_runs_keep_reaction_hooks_and_terminal_states_separate()
-> Result<(), Box<dyn std::error::Error>> {
    let started = Arc::new(tokio::sync::Notify::new());
    let finish = Arc::new(tokio::sync::Notify::new());
    let model = MockCompletionModel::from_stream_turns([
        vec![
            MockStreamEvent::tool_call("tc1", "controlled", serde_json::json!({})),
            MockStreamEvent::final_response(Default::default()),
        ],
        text_turn("second answer"),
        text_turn("first answer"),
    ]);
    let agent = AgentBuilder::new(model)
        .tool(rig_agent::test_utils::MockControlledTool::new(
            started.clone(),
            finish.clone(),
        ))
        .default_max_turns(3)
        .memory(InMemoryConversationMemory::new())
        .build();
    let router = Arc::new(ChatRouter::new(
        agent,
        Gate::default(),
        ChatConfig::default(),
    ));
    let adapter = fake_adapter();
    let first_input = inbound("first");
    let mut second_input = inbound("second");
    second_input.message.channel.channel_id = "other".into();
    second_input.reply_channel = second_input.message.channel.clone();
    let first_message = first_input.message.clone();
    let second_message = second_input.message.clone();
    let first = tokio::spawn({
        let router = router.clone();
        let adapter = adapter.clone();
        async move { router.handle(adapter, first_input, "bot").await }
    });
    started.notified().await;
    advance(700).await;
    assert!(
        adapter
            .calls()
            .contains(&Call::Add(first_message.clone(), "🔥".into()))
    );
    router.handle(adapter.clone(), second_input, "bot").await?;
    assert!(!first.is_finished());
    assert!(
        adapter
            .calls()
            .contains(&Call::Add(second_message.clone(), "🆗".into()))
    );
    assert!(
        !adapter
            .calls()
            .contains(&Call::Add(first_message.clone(), "🆗".into()))
    );
    let second_reactions = || {
        adapter.calls().into_iter().filter(|call| {
            matches!(call, Call::Add(message, _) | Call::Remove(message, _) if message == &second_message)
        }).collect::<Vec<_>>()
    };
    let completed_second = second_reactions();
    finish.notify_one();
    first.await??;
    assert!(
        adapter
            .calls()
            .contains(&Call::Add(first_message, "🆗".into()))
    );
    assert_eq!(second_reactions(), completed_second);
    Ok(())
}
