use rig_core::error::ProviderError;

/// Failures in configuration, original storage, or search projections.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Configuration or returned projection data violates a store invariant.
    #[error("Conversation store configuration or invariant failed: {0}")]
    Configuration(String),
    /// SQLite could not persist or retrieve authoritative state.
    #[error("Conversation original storage failed: {source}")]
    Storage {
        /// Original storage failure.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A serialized original or projection payload could not be encoded.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The embedding provider failed.
    #[error(transparent)]
    Embedding(#[from] ProviderError),
    /// Qdrant failed to read or write its projection.
    #[error(transparent)]
    Qdrant(#[from] qdrant_client::QdrantError),
    /// A bounded owned processing task could not finish.
    #[error(transparent)]
    Task(#[from] tokio::task::JoinError),
}

impl From<crate::storage::StorageError> for StoreError {
    fn from(source: crate::storage::StorageError) -> Self {
        Self::Storage {
            source: Box::new(source),
        }
    }
}
