//! Object-safe outbound platform operations.
//!
//! ```
//! use rig_messaging::ChatAdapter;
//! fn supports_preview(adapter: &dyn ChatAdapter) -> bool { adapter.supports_edit() }
//! ```

use crate::{ChannelRef, MessageRef};
use rig_core::wasm_compat::{WasmBoxedFuture, WasmCompatSend, WasmCompatSync};

/// Transport, stream or routing failure.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    /// The adapter cannot perform this operation.
    #[error("unsupported operation: {0}")]
    Unsupported(&'static str),
    /// A platform API failed.
    #[error("platform operation failed: {0}")]
    #[cfg(not(target_family = "wasm"))]
    Platform(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A platform API failed.
    #[cfg(target_family = "wasm")]
    #[error("platform operation failed: {0}")]
    Platform(#[source] Box<dyn std::error::Error>),
    /// The adapter advertises an invalid message limit.
    #[error("message limit must be positive")]
    InvalidMessageLimit,
    /// The agent stream failed.
    #[error("{0}")]
    Stream(#[from] rig_agent::agent::StreamingError),
    /// The stream ended without a terminal item.
    #[error("agent stream ended without a final response")]
    UnexpectedEnd,
    /// The memory backend did not acknowledge persistence.
    #[error("history persistence was not acknowledged: {0}")]
    MemoryAppend(rig_core::error::ErrorReport),
    /// The Agent had no conversation-memory append outcome.
    #[error("agent must have conversation memory configured")]
    MissingMemory,
}

/// Outbound operations. Message limits count Unicode scalar values and must be positive.
/// Capabilities describe whether preview edits and status reactions are available.
pub trait ChatAdapter: WasmCompatSend + WasmCompatSync + 'static {
    /// Platform identifier used in message addresses.
    fn platform(&self) -> &'static str;
    /// Maximum Unicode scalar values per message. Must be positive.
    fn message_limit(&self) -> usize;
    /// Send text and return its platform address.
    fn send<'a>(
        &'a self,
        ch: &'a ChannelRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>>;
    /// Replace text. Return `Unsupported` when edits are unavailable.
    fn edit<'a>(
        &'a self,
        m: &'a MessageRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>>;
    /// Delete an existing message.
    fn delete<'a>(&'a self, m: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>>;
    /// Add the bot's reaction to an existing message.
    fn add_reaction<'a>(
        &'a self,
        m: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>>;
    /// Remove the bot's reaction from an existing message.
    fn remove_reaction<'a>(
        &'a self,
        m: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>>;
    /// Whether preview edits are available.
    fn supports_edit(&self) -> bool {
        true
    }
    /// Whether status reactions are available.
    fn supports_reactions(&self) -> bool {
        true
    }
    /// Whether markdown tables can pass through unchanged.
    fn renders_native_tables(&self) -> bool {
        false
    }
}
