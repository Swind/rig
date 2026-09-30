use crate::*;
use rig_core::wasm_compat::WasmBoxedFuture;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    Send(ChannelRef, String),
    Edit(MessageRef, String),
    Delete(MessageRef),
    Add(MessageRef, String),
    Remove(MessageRef, String),
}

pub(crate) struct FakeAdapter {
    pub(crate) calls: Mutex<Vec<Call>>,
    pub edits: bool,
    pub reactions: bool,
    pub native_tables: bool,
    pub limit: usize,
    pub fail_sends: AtomicUsize,
    pub fail_edits: AtomicUsize,
    pub fail_deletes: AtomicBool,
    pub fail_reactions: AtomicUsize,
    pub reaction_block: Option<std::sync::Arc<tokio::sync::Notify>>,
    pub(crate) counter: AtomicUsize,
}

impl Default for FakeAdapter {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            edits: true,
            reactions: false,
            native_tables: false,
            limit: 2000,
            fail_sends: AtomicUsize::new(0),
            fail_edits: AtomicUsize::new(0),
            fail_deletes: AtomicBool::new(false),
            fail_reactions: AtomicUsize::new(0),
            reaction_block: None,
            counter: AtomicUsize::new(0),
        }
    }
}
impl FakeAdapter {
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn record(&self, call: Call) {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(call);
    }
    fn fails(counter: &AtomicUsize) -> bool {
        counter
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }
    fn error() -> ChatError {
        ChatError::Platform(Box::new(std::io::Error::other(
            "simulated transport failure",
        )))
    }
}
impl ChatAdapter for FakeAdapter {
    fn platform(&self) -> &'static str {
        "fake"
    }
    fn message_limit(&self) -> usize {
        self.limit
    }
    fn supports_edit(&self) -> bool {
        self.edits
    }
    fn supports_reactions(&self) -> bool {
        self.reactions
    }
    fn renders_native_tables(&self) -> bool {
        self.native_tables
    }
    fn send<'a>(
        &'a self,
        ch: &'a ChannelRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>> {
        Box::pin(async move {
            self.record(Call::Send(ch.clone(), text.into()));
            if Self::fails(&self.fail_sends) {
                return Err(Self::error());
            }
            Ok(MessageRef {
                channel: ch.clone(),
                message_id: self.counter.fetch_add(1, Ordering::SeqCst).to_string(),
            })
        })
    }
    fn edit<'a>(
        &'a self,
        m: &'a MessageRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.record(Call::Edit(m.clone(), text.into()));
            if !self.edits {
                return Err(ChatError::Unsupported("edit"));
            }
            if Self::fails(&self.fail_edits) {
                Err(Self::error())
            } else {
                Ok(())
            }
        })
    }
    fn delete<'a>(&'a self, m: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.record(Call::Delete(m.clone()));
            if self.fail_deletes.load(Ordering::SeqCst) {
                Err(Self::error())
            } else {
                Ok(())
            }
        })
    }
    fn add_reaction<'a>(
        &'a self,
        m: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.record(Call::Add(m.clone(), emoji.into()));
            if let Some(block) = &self.reaction_block {
                block.notified().await;
            }
            if Self::fails(&self.fail_reactions) {
                Err(Self::error())
            } else {
                Ok(())
            }
        })
    }
    fn remove_reaction<'a>(
        &'a self,
        m: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.record(Call::Remove(m.clone(), emoji.into()));
            if Self::fails(&self.fail_reactions) {
                Err(Self::error())
            } else {
                Ok(())
            }
        })
    }
}

pub(crate) fn inbound(text: &str) -> Inbound {
    let channel = ChannelRef {
        platform: "fake".into(),
        scope_id: None,
        channel_id: "room".into(),
        thread_id: None,
    };
    Inbound {
        message: MessageRef {
            channel: channel.clone(),
            message_id: text.into(),
        },
        reply_channel: channel,
        sender: Sender {
            id: "alice-id".into(),
            name: "alice".into(),
            is_bot: false,
        },
        text: text.into(),
        attachments: vec![],
        is_dm: true,
        is_thread: false,
        mentions_bot: false,
    }
}

pub(crate) fn text_turn(text: &str) -> Vec<rig_core::test_utils::MockStreamEvent> {
    use rig_core::test_utils::MockStreamEvent;
    vec![
        MockStreamEvent::text(text),
        MockStreamEvent::final_response(Default::default()),
    ]
}
