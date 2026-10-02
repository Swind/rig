//! Runs normalized stdin lines through messaging routing, memory and final delivery.
//!
//! Requires `OPENAI_API_KEY`. `OPENAI_MODEL` defaults to `gpt-4o-mini`.
//! Replies print after each completed turn. This is a development harness.
//! Use Rig's `ChatBotBuilder` for a terminal interface that displays each token.

use rig_agent::AgentBuilder;
use rig_core::{
    memory::InMemoryConversationMemory,
    providers::openai::{self, OpenAI},
    wasm_compat::{WasmBoxedFuture, WasmCompatSend, WasmCompatSync},
};
use rig_messaging::{
    ChannelRef, ChatAdapter, ChatConfig, ChatError, ChatRouter, Gate, Inbound, MessageRef, Sender,
};
use std::{
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};

struct StdioAdapter<W> {
    writer: Mutex<W>,
    next_message: AtomicUsize,
    #[cfg(test)]
    unexpected_calls: AtomicUsize,
}
impl<W> StdioAdapter<W> {
    fn new(writer: W) -> Self {
        Self {
            writer: Mutex::new(writer),
            next_message: AtomicUsize::new(0),
            #[cfg(test)]
            unexpected_calls: AtomicUsize::new(0),
        }
    }
    fn unsupported(&self, operation: &'static str) -> ChatError {
        #[cfg(test)]
        self.unexpected_calls.fetch_add(1, Ordering::SeqCst);
        ChatError::Unsupported(operation)
    }
}
impl<W: Write + WasmCompatSend + WasmCompatSync + 'static> ChatAdapter for StdioAdapter<W> {
    fn platform(&self) -> &'static str {
        "stdio"
    }
    fn message_limit(&self) -> usize {
        usize::MAX
    }
    fn supports_edit(&self) -> bool {
        false
    }
    fn supports_reactions(&self) -> bool {
        false
    }
    fn send<'a>(
        &'a self,
        ch: &'a ChannelRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>> {
        Box::pin(async move {
            let mut writer = self.writer.lock().map_err(|_| {
                ChatError::Platform(Box::new(std::io::Error::other("stdout lock poisoned")))
            })?;
            writeln!(writer, "{text}").map_err(|error| ChatError::Platform(Box::new(error)))?;
            writer
                .flush()
                .map_err(|error| ChatError::Platform(Box::new(error)))?;
            Ok(MessageRef {
                channel: ch.clone(),
                message_id: self.next_message.fetch_add(1, Ordering::SeqCst).to_string(),
            })
        })
    }
    fn edit<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move { Err(self.unsupported("edit")) })
    }
    fn delete<'a>(&'a self, _: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move { Err(self.unsupported("delete")) })
    }
    fn add_reaction<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move { Err(self.unsupported("add_reaction")) })
    }
    fn remove_reaction<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move { Err(self.unsupported("remove_reaction")) })
    }
}

async fn run_lines(
    input: impl AsyncBufRead + Unpin,
    router: &ChatRouter,
    adapter: Arc<dyn ChatAdapter>,
    name: String,
) -> std::io::Result<()> {
    let mut lines = input.lines();
    let mut next_input = 0_u64;
    while let Some(text) = lines.next_line().await? {
        let channel = ChannelRef {
            platform: "stdio".into(),
            scope_id: None,
            channel_id: "local".into(),
            thread_id: None,
        };
        let inbound = Inbound {
            message: MessageRef {
                channel: channel.clone(),
                message_id: next_input.to_string(),
            },
            reply_channel: channel,
            sender: Sender {
                id: "local".into(),
                name: name.clone(),
                is_bot: false,
            },
            text,
            attachments: vec![],
            is_dm: true,
            is_thread: false,
            mentions_bot: false,
            context: rig_messaging::MessageContext::default(),
        };
        next_input += 1;
        if let Err(error) = router.handle(adapter.clone(), inbound, "rig-bot").await {
            eprintln!("{error}");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let provider = OpenAI::from_env()?;
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| openai::GPT_4O_MINI.into());
    let agent = AgentBuilder::new(provider.completion(model))
        .memory(InMemoryConversationMemory::new())
        .build();
    let router = ChatRouter::new(agent, Gate::default(), ChatConfig::default());
    let adapter = Arc::new(StdioAdapter::new(std::io::stdout()));
    run_lines(
        BufReader::new(tokio::io::stdin()),
        &router,
        adapter,
        std::env::var("USER").unwrap_or_else(|_| "user".into()),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
