//! Authoritative SQLite conversation history with a searchable Qdrant
//! projection. Applications explicitly process durable indexing work.
//!
//! ```no_run
//! # async fn example(store: rig_conversation_store::ConversationStore) -> Result<(), Box<dyn std::error::Error>> {
//! use rig_core::{completion::Message, memory::ConversationMemory, tool::builtin::SearchConversationsTool};
//! store.append(&"thread-1".into(), vec![Message::user("Discuss Cypher")]).await?;
//! store.process_pending(20).await?;
//! let search_tool = SearchConversationsTool::new(store.clone());
//! # let _ = search_tool;
//! # Ok(()) }
//! ```

#[cfg(target_family = "wasm")]
compile_error!("rig-conversation-store is a native-only conversation backend");

#[cfg(not(target_family = "wasm"))]
mod chunking;
#[cfg(not(target_family = "wasm"))]
mod error;
#[cfg(not(target_family = "wasm"))]
mod indexer;
#[cfg(not(target_family = "wasm"))]
mod qdrant;
#[cfg(not(target_family = "wasm"))]
mod search;
#[cfg(not(target_family = "wasm"))]
mod storage;

#[cfg(not(target_family = "wasm"))]
mod native;
#[cfg(not(target_family = "wasm"))]
pub use error::StoreError;
#[cfg(not(target_family = "wasm"))]
pub use native::{ConversationStore, IndexStatus, StoreConfig};
