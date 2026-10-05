//! Agent tool for searching saved conversation excerpts through a scoped backend.
//!
//! ```
//! use rig_core::{conversation_search::ConversationSearch, tool::builtin::SearchConversationsTool};
//!
//! fn search_tool<S: ConversationSearch>(backend: S) -> SearchConversationsTool<S> {
//!     SearchConversationsTool::new(backend)
//! }
//! ```

use serde_json::{Value, json};

use crate::{
    conversation_search::{
        ConversationSearch, ConversationSearchError, ConversationSearchHit,
        ConversationSearchRequest,
    },
    tool::PortableTool,
};

/// Searches saved conversations using a backend with application-defined access scope.
pub struct SearchConversationsTool<S> {
    search: S,
}

impl<S: ConversationSearch> SearchConversationsTool<S> {
    /// Wraps a backend that owns authorization, ranking, and original-message retrieval.
    pub fn new(search: S) -> Self {
        Self { search }
    }
}

impl<S: ConversationSearch> PortableTool for SearchConversationsTool<S> {
    const NAME: &'static str = "search_conversations";
    type Args = ConversationSearchRequest;
    type Output = Vec<ConversationSearchHit>;
    type Error = ConversationSearchError;

    fn description(&self) -> String {
        "Search saved conversations for relevant original message excerpts. Optionally narrow the search to one conversation within the available access scope."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Nonblank text to search for in saved conversations."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of conversation excerpts to return.",
                    "default": 5,
                    "minimum": 1,
                    "maximum": 100
                },
                "conversation_id": {
                    "type": ["string", "null"],
                    "description": "Optional conversation ID that narrows the search scope."
                }
            },
            "required": ["query"]
        })
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        args.validate()?;
        let limit = args.limit as usize;
        let mut hits = self.search.search(args).await?;
        hits.truncate(limit);
        Ok(hits)
    }
}

#[cfg(test)]
mod tests;
