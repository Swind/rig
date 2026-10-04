#![allow(clippy::panic_in_result_fn)]

use super::*;
use crate::{
    Attachment,
    test_support::{Call, FakeAdapter, inbound, text_turn},
};
use rig_agent::{
    AgentBuilder,
    agent::{AgentHook, HookContext, ObservationAction, TextDelta},
};
use rig_core::{
    memory::{ConversationMemory, InMemoryConversationMemory},
    test_utils::{AppendFailingMemory, MockCompletionModel},
};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

struct HoldFirstDelta {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    first: AtomicBool,
}
impl AgentHook for HoldFirstDelta {
    async fn on_text_delta(&self, _: &HookContext, _: TextDelta<'_>) -> ObservationAction {
        if self.first.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        ObservationAction::Continue
    }
}

#[tokio::test]
async fn same_session_waits_and_loads_complete_history() -> Result<(), Box<dyn std::error::Error>> {
    let memory = InMemoryConversationMemory::new();
    let model = MockCompletionModel::from_stream_turns([
        text_turn("first answer"),
        text_turn("second answer"),
    ]);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let agent = AgentBuilder::new(model.clone())
        .memory(memory.clone())
        .add_hook(HoldFirstDelta {
            entered: entered.clone(),
            release: release.clone(),
            first: AtomicBool::new(true),
        })
        .build();
    let router = Arc::new(ChatRouter::new(
        agent,
        Gate::default(),
        ChatConfig::default(),
    ));
    let adapter = Arc::new(FakeAdapter {
        reactions: true,
        ..Default::default()
    });
    let first = tokio::spawn({
        let router = router.clone();
        let adapter = adapter.clone();
        async move { router.handle(adapter, inbound("first"), "bot").await }
    });
    entered.notified().await;
    let second = tokio::spawn({
        let router = router.clone();
        let adapter = adapter.clone();
        async move { router.handle(adapter, inbound("second"), "bot").await }
    });
    // Wait until both handlers have cloned the same lock, while the first is held.
    loop {
        let locks = router.locks.lock().await;
        if locks.values().any(|lock| Arc::strong_count(lock) == 3) {
            break;
        }
        drop(locks);
        tokio::task::yield_now().await;
    }
    assert_eq!(model.request_count(), 1);
    assert!(
        adapter
            .calls()
            .contains(&Call::Add(inbound("second").message, "👀".into()))
    );
    release.notify_one();
    first.await??;
    second.await??;
    let requests = model.requests();
    assert!(
        requests[1]
            .chat_history
            .contains(&Message::assistant("first answer"))
    );
    assert!(
        requests[1]
            .chat_history
            .contains(&Message::user(inbound("first").prompt_text()))
    );
    assert_eq!(
        memory
            .load(&inbound("first").reply_channel.session_key().into())
            .await?
            .len(),
        4
    );
    assert!(router.locks.lock().await.is_empty());
    Ok(())
}

#[tokio::test]
async fn transport_failures_still_append_memory_and_clean_locks()
-> Result<(), Box<dyn std::error::Error>> {
    let model =
        MockCompletionModel::from_stream_turns([text_turn("persist me"), text_turn("next")]);
    let agent = AgentBuilder::new(model.clone())
        .memory(InMemoryConversationMemory::new())
        .build();
    let router = ChatRouter::new(agent, Gate::default(), ChatConfig::default());
    let adapter = Arc::new(FakeAdapter::default());
    adapter.fail_sends.store(usize::MAX, Ordering::SeqCst);
    adapter.fail_edits.store(usize::MAX, Ordering::SeqCst);
    assert!(
        router
            .handle(adapter.clone(), inbound("failed delivery"), "bot")
            .await
            .is_err()
    );
    assert!(router.locks.lock().await.is_empty());
    adapter.fail_sends.store(0, Ordering::SeqCst);
    adapter.fail_edits.store(0, Ordering::SeqCst);
    router.handle(adapter, inbound("next"), "bot").await?;
    assert!(
        model.requests()[1]
            .chat_history
            .contains(&Message::assistant("persist me"))
    );
    Ok(())
}

struct CountAppends(Arc<std::sync::atomic::AtomicUsize>);
impl AgentHook for CountAppends {
    fn observes(&self, _: rig_agent::agent::StepEventKind) -> bool {
        true
    }
    async fn on_dispatch(
        &self,
        _: &HookContext,
        event: rig_agent::agent::DispatchEvent<'_>,
    ) -> rig_agent::agent::DispatchAction {
        if matches!(
            event.kind,
            rig_core::effect::EffectKind::Memory {
                op: rig_core::effect::MemoryOp::Append { .. }
            }
        ) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        rig_agent::agent::DispatchAction::Proceed
    }
}

#[tokio::test]
async fn append_failure_warns_without_losing_answer() {
    let appends = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let agent = AgentBuilder::new(MockCompletionModel::from_stream_turns([text_turn(
        "answer",
    )]))
    .memory(AppendFailingMemory::default())
    .add_hook(CountAppends(appends.clone()))
    .build();
    let router = ChatRouter::new(agent, Gate::default(), ChatConfig::default());
    let adapter = Arc::new(FakeAdapter::default());
    assert!(matches!(
        router.handle(adapter.clone(), inbound("go"), "bot").await,
        Err(ChatError::MemoryAppend(_))
    ));
    assert!(adapter.calls().iter().any(|call| matches!(call, Call::Edit(_,text) if text.contains("answer") && text.contains("persistence"))));
    assert!(router.locks.lock().await.is_empty());
    assert_eq!(appends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rejected_input_performs_no_platform_or_model_operations() -> Result<(), ChatError> {
    let model = MockCompletionModel::from_stream_turns([text_turn("answer")]);
    let router = ChatRouter::new(
        AgentBuilder::new(model.clone())
            .memory(InMemoryConversationMemory::new())
            .build(),
        Gate::default(),
        ChatConfig::default(),
    );
    let adapter = Arc::new(FakeAdapter::default());
    let mut m = inbound("go");
    m.is_dm = false;
    router.handle(adapter.clone(), m, "bot").await?;
    assert!(adapter.calls().is_empty());
    assert_eq!(model.request_count(), 0);
    let invalid = Arc::new(FakeAdapter {
        limit: 0,
        ..Default::default()
    });
    assert!(matches!(
        router.handle(invalid.clone(), inbound("go"), "bot").await,
        Err(ChatError::InvalidMessageLimit)
    ));
    assert!(invalid.calls().is_empty());
    assert_eq!(model.request_count(), 0);
    Ok(())
}

#[test]
fn prompt_includes_display_context_and_converts_only_supported_media() {
    let mut m = inbound("hello");
    m.attachments = vec![
        Attachment {
            filename: "image.png".into(),
            mime: "image/png".into(),
            size: Some(3),
            source: AttachmentSource::Bytes(bytes::Bytes::from_static(b"png")),
        },
        Attachment {
            filename: "file.bin".into(),
            mime: "application/octet-stream".into(),
            size: None,
            source: AttachmentSource::Url("https://example.com/file".into()),
        },
    ];
    let cfg = ChatConfig::default();
    let prompt = build_prompt(&m, &cfg);
    let content = match prompt {
        Message::User { content } => content,
        _ => Vec::new(),
    };
    assert_eq!(content.len(), 3);
    assert_eq!(content[0], UserContent::text(m.prompt_text()));
    assert!(
        matches!(&content[1],UserContent::Image(image) if matches!(&image.data,rig_core::message::DocumentSourceKind::Raw(data) if data==b"png"))
    );
    assert_eq!(
        content[2],
        UserContent::text("[Attachment unavailable: file.bin (application/octet-stream)]")
    );
}

#[tokio::test]
async fn independent_session_runs_while_first_is_waiting() -> Result<(), Box<dyn std::error::Error>>
{
    let model =
        MockCompletionModel::from_stream_turns([text_turn("held"), text_turn("independent")]);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let agent = AgentBuilder::new(model.clone())
        .memory(InMemoryConversationMemory::new())
        .add_hook(HoldFirstDelta {
            entered: entered.clone(),
            release: release.clone(),
            first: AtomicBool::new(true),
        })
        .build();
    let router = Arc::new(ChatRouter::new(
        agent,
        Gate::default(),
        ChatConfig::default(),
    ));
    let adapter = Arc::new(FakeAdapter::default());
    let first = tokio::spawn({
        let router = router.clone();
        let adapter = adapter.clone();
        async move { router.handle(adapter, inbound("first"), "bot").await }
    });
    entered.notified().await;
    let mut second = inbound("second");
    second.reply_channel.channel_id = "another".into();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        router.handle(adapter, second, "bot"),
    )
    .await??;
    assert_eq!(model.request_count(), 2);
    assert_eq!(
        model.requests()[1].chat_history,
        vec![Message::user(inbound("second").prompt_text())]
    );
    release.notify_one();
    first.await??;
    assert!(router.locks.lock().await.is_empty());
    Ok(())
}

#[test]
fn attachment_conversion_supports_each_media_kind_and_source() {
    let mut m = inbound("media");
    let mime_types = ["image/png", "application/pdf", "audio/mp3", "video/mp4"];
    for mime in mime_types {
        for source in [
            AttachmentSource::Bytes(bytes::Bytes::from_static(b"data")),
            AttachmentSource::Url("https://example.com/media".into()),
        ] {
            m.attachments.push(Attachment {
                filename: "media".into(),
                mime: mime.into(),
                size: None,
                source,
            });
        }
    }
    let cfg = ChatConfig::default();
    let Message::User { content } = build_prompt(&m, &cfg) else {
        return;
    };
    assert_eq!(content.len(), 9);
    for (i, item) in content.iter().skip(1).enumerate() {
        let data = match item {
            UserContent::Image(v) => Some(&v.data),
            UserContent::Document(v) => Some(&v.data),
            UserContent::Audio(v) => Some(&v.data),
            UserContent::Video(v) => Some(&v.data),
            _ => None,
        };
        assert!(data.is_some());
        if i % 2 == 0 {
            assert!(
                matches!(data,Some(rig_core::message::DocumentSourceKind::Raw(bytes)) if bytes==b"data")
            );
        } else {
            assert!(
                matches!(data,Some(rig_core::message::DocumentSourceKind::Url(url)) if url=="https://example.com/media")
            );
        }
    }
}

#[test]
fn attachment_allowlist_restricts_recognized_media_and_can_disable_it() {
    let mut m = inbound("media");
    m.attachments = vec![
        Attachment {
            filename: "image.png".into(),
            mime: "image/png".into(),
            size: None,
            source: AttachmentSource::Bytes(bytes::Bytes::from_static(b"png")),
        },
        Attachment {
            filename: "file.pdf".into(),
            mime: "application/pdf".into(),
            size: None,
            source: AttachmentSource::Url("https://example.com/file.pdf".into()),
        },
    ];
    for types in [HashSet::from(["image/png".into()]), HashSet::new()] {
        let accepts_image = types.contains("image/png");
        let cfg = ChatConfig {
            attachment_mime_types: Some(types),
            ..Default::default()
        };
        let content = match build_prompt(&m, &cfg) {
            Message::User { content } => content,
            _ => Vec::new(),
        };
        assert_eq!(content.len(), 3);
        if accepts_image {
            assert!(matches!(&content[1], UserContent::Image(_)));
        } else {
            assert_eq!(
                content[1],
                UserContent::text("[Attachment unavailable: image.png (image/png)]")
            );
        }
        assert_eq!(
            content[2],
            UserContent::text("[Attachment unavailable: file.pdf (application/pdf)]")
        );
    }
}

struct HoldEachDelta {
    entered: Arc<[Notify; 3]>,
    release: Arc<[Notify; 3]>,
    next: std::sync::atomic::AtomicUsize,
}
impl AgentHook for HoldEachDelta {
    async fn on_text_delta(&self, _: &HookContext, _: TextDelta<'_>) -> ObservationAction {
        let index = self.next.fetch_add(1, Ordering::SeqCst);
        if let (Some(entered), Some(release)) = (self.entered.get(index), self.release.get(index)) {
            entered.notify_one();
            release.notified().await;
        }
        ObservationAction::Continue
    }
}

#[tokio::test]
async fn arrival_during_cleanup_keeps_the_waiting_turns_lock()
-> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let model = MockCompletionModel::from_stream_turns([
            text_turn("first answer"),
            text_turn("second answer"),
            text_turn("third answer"),
        ]);
        let entered = Arc::new(std::array::from_fn(|_| Notify::new()));
        let release = Arc::new(std::array::from_fn(|_| Notify::new()));
        let agent = AgentBuilder::new(model.clone())
            .memory(InMemoryConversationMemory::new())
            .add_hook(HoldEachDelta {
                entered: entered.clone(),
                release: release.clone(),
                next: std::sync::atomic::AtomicUsize::new(0),
            })
            .build();
        let mut cfg = ChatConfig::default();
        cfg.reactions.enabled = false;
        let router = Arc::new(ChatRouter::new(agent, Gate::default(), cfg));
        let adapter = Arc::new(FakeAdapter::default());
        let spawn = |text: &'static str| {
            let router = router.clone();
            let adapter = adapter.clone();
            tokio::spawn(async move { router.handle(adapter, inbound(text), "bot").await })
        };
        let key = inbound("first").reply_channel.session_key();
        let first = spawn("first");
        entered[0].notified().await;
        let original = Arc::downgrade(
            router
                .locks
                .lock()
                .await
                .get(&key)
                .ok_or("missing first lock")?,
        );
        let second = spawn("second");
        loop {
            let locks = router.locks.lock().await;
            if locks
                .get(&key)
                .is_some_and(|lock| Arc::strong_count(lock) == 3)
            {
                break;
            }
            drop(locks);
            tokio::task::yield_now().await;
        }
        // Block cleanup while the first guard is released and the second turn starts.
        let table = router.locks.lock().await;
        release[0].notify_one();
        entered[1].notified().await;
        assert!(!first.is_finished());
        let third = spawn("third");
        tokio::task::yield_now().await;
        assert_eq!(model.request_count(), 2);
        drop(table);
        first.await??;
        loop {
            let locks = router.locks.lock().await;
            if locks
                .get(&key)
                .is_some_and(|lock| Arc::strong_count(lock) == 3)
            {
                assert!(std::sync::Weak::ptr_eq(
                    &original,
                    &Arc::downgrade(locks.get(&key).ok_or("missing active lock")?),
                ));
                break;
            }
            drop(locks);
            tokio::task::yield_now().await;
        }
        assert_eq!(model.request_count(), 2);
        release[1].notify_one();
        entered[2].notified().await;
        release[2].notify_one();
        second.await??;
        third.await??;
        let requests = model.requests();
        assert!(
            requests[2]
                .chat_history
                .contains(&Message::assistant("first answer"))
        );
        assert!(
            requests[2]
                .chat_history
                .contains(&Message::assistant("second answer"))
        );
        assert!(router.locks.lock().await.is_empty());
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await?
}
