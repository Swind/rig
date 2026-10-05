//! Search saved conversations and return original message excerpts with source
//! references. Backends own authorization, ranking, and excerpt retrieval.
//!
//! ```
//! use rig_core::conversation_search::ConversationSearchRequest;
//!
//! let request: ConversationSearchRequest =
//!     serde_json::from_str(r#"{"query":"Cypher design"}"#)?;
//! request.validate()?;
//! assert_eq!(request.limit, 5);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::{error::Error, future::Future};

use serde::{Deserialize, Serialize};

use crate::{
    completion::Message,
    id::ConversationId,
    wasm_compat::{WasmCompatSend, WasmCompatSync},
};

#[cfg(not(target_family = "wasm"))]
type BoxedError = Box<dyn Error + Send + Sync + 'static>;
#[cfg(target_family = "wasm")]
type BoxedError = Box<dyn Error + 'static>;

fn default_limit() -> u32 {
    5
}

/// Search text, a result-count bound, and an optional conversation filter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationSearchRequest {
    /// Search text, preserved without trimming or other transformations.
    pub query: String,
    /// Maximum number of excerpts to return, from 1 through 100. Defaults to 5.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Narrows the backend's authorized scope to this conversation when supplied.
    #[serde(default)]
    pub conversation_id: Option<ConversationId>,
}

impl ConversationSearchRequest {
    /// Rejects whitespace-only queries and limits outside `1..=100`.
    ///
    /// Returns [`ConversationSearchError::InvalidRequest`] without changing the request.
    pub fn validate(&self) -> Result<(), ConversationSearchError> {
        if self.query.trim().is_empty() {
            return Err(ConversationSearchError::InvalidRequest {
                reason: "query must contain non-whitespace text".into(),
            });
        }
        if !(1..=100).contains(&self.limit) {
            return Err(ConversationSearchError::InvalidRequest {
                reason: "limit must be between 1 and 100".into(),
            });
        }
        Ok(())
    }
}

/// A contiguous original excerpt with a stable reference to its saved conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationSearchHit {
    /// The saved conversation containing the excerpt.
    pub conversation_id: ConversationId,
    /// Backend-owned stable chunk reference scoped to the conversation.
    pub chunk_id: String,
    /// Zero-based position of the first message in the saved conversation.
    pub start_index: u64,
    /// Original messages in source order, preserving complete tool-call/result exchanges.
    pub messages: Vec<Message>,
}

/// Searches saved conversations within a backend's authorized scope.
///
/// Implementations validate requests, enforce the optional conversation filter
/// and result limit, and hydrate original excerpts with complete tool exchanges.
/// Search does not modify conversation history or inject messages into an agent.
pub trait ConversationSearch: WasmCompatSend + WasmCompatSync {
    /// Returns excerpts in backend relevance order, or an empty vector for no matches.
    ///
    /// Invalid requests return [`ConversationSearchError::InvalidRequest`]. Backend
    /// failures return [`ConversationSearchError::Backend`] with their original source.
    fn search(
        &self,
        request: ConversationSearchRequest,
    ) -> impl Future<Output = Result<Vec<ConversationSearchHit>, ConversationSearchError>> + WasmCompatSend;
}

/// Request-validation and backend failures from conversation search.
#[derive(Debug, thiserror::Error)]
pub enum ConversationSearchError {
    /// The request violates the shared search constraints.
    #[error("Invalid conversation search request: {reason}")]
    InvalidRequest {
        /// The violated constraint.
        reason: String,
    },
    /// The backend could not search or retrieve the original excerpt.
    #[error("Conversation search backend failed: {source}")]
    Backend {
        /// The original backend error.
        source: BoxedError,
    },
}

impl ConversationSearchError {
    /// Wraps a backend failure while preserving its source.
    pub fn backend(source: impl Error + WasmCompatSend + WasmCompatSync + 'static) -> Self {
        Self::Backend {
            source: Box::new(source),
        }
    }
}

#[cfg(test)]
mod tests;
