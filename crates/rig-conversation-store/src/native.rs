use crate::{StoreError, qdrant, storage};

use std::{path::Path, sync::Arc};

use rig_core::{
    DynModel,
    completion::Message,
    id::ConversationId,
    memory::{ConversationMemory, MemoryError},
    operation::Embedding,
    wasm_compat::WasmBoxedFuture,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// Access scope and bounded indexing/search configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreConfig {
    /// Application-owned access scope shared by memory and search handles.
    pub scope: String,
    /// Stable embedding model/version identity supplied by the application.
    pub model_identity: String,
    /// Expected length of every indexed and query embedding.
    pub vector_dimension: u64,
    /// Maximum UTF-8 bytes sent when embedding one original chunk.
    pub max_embedding_bytes: usize,
    /// Maximum UTF-8 bytes accepted in a search query.
    pub max_query_bytes: usize,
    /// Maximum Qdrant candidates inspected, including stale candidates.
    pub max_candidates: usize,
    /// Maximum adjacent SQLite chunk references inspected per search.
    pub max_context_chunks: usize,
    /// Maximum serialized original-hit bytes hydrated per search.
    pub max_output_bytes: usize,
}

impl StoreConfig {
    /// Creates bounded defaults for a scope, embedding model identity, and dimension.
    pub fn new(
        scope: impl Into<String>,
        model_identity: impl Into<String>,
        dimension: u64,
    ) -> Self {
        Self {
            scope: scope.into(),
            model_identity: model_identity.into(),
            vector_dimension: dimension,
            max_embedding_bytes: 16 * 1024,
            max_query_bytes: 16 * 1024,
            max_candidates: 400,
            max_context_chunks: 300,
            max_output_bytes: 256 * 1024,
        }
    }

    fn validate(&self) -> Result<(), StoreError> {
        if self.scope.trim().is_empty()
            || self.model_identity.trim().is_empty()
            || self.vector_dimension == 0
            || self.vector_dimension > 65536
            || self.max_embedding_bytes == 0
            || self.max_embedding_bytes > 1024 * 1024
            || self.max_query_bytes == 0
            || self.max_query_bytes > 1024 * 1024
            || !(1..=10000).contains(&self.max_candidates)
            || !(1..=10000).contains(&self.max_context_chunks)
            || self.max_output_bytes < 2
            || self.max_output_bytes > 16 * 1024 * 1024
        {
            return Err(StoreError::Configuration("scope/model identity must be nonblank and dimensions/budgets must be bounded positive values".into()));
        }
        Ok(())
    }
}

/// Durable projection progress for this handle's scope.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexStatus {
    /// Chunks with indexing or cleanup work remaining.
    pub pending_jobs: u64,
    /// Pending Qdrant operations.
    pub pending_vector: u64,
    /// Recorded failed processing attempts for pending jobs.
    pub failed_attempts: u64,
    /// A retained pending processing failure, when available.
    pub last_error: Option<String>,
}

pub(crate) struct Inner {
    pub(crate) storage: storage::Storage,
    pub(crate) vector: qdrant::VectorProjection,
    pub(crate) model: DynModel<Embedding>,
    pub(crate) config: StoreConfig,
    pub(crate) projection_gate: Mutex<()>,
}

/// Cloneable scoped memory and search backend sharing authoritative originals.
#[derive(Clone)]
pub struct ConversationStore {
    pub(crate) inner: Arc<Inner>,
}

impl ConversationStore {
    /// Opens persistent originals and checks the Qdrant collection.
    ///
    /// Index/model incompatibility and unavailable external stores return errors.
    /// Clones share the projection/clear gate. Independently opened handles must
    /// not concurrently index or clear the same scope; use one store per scope.
    pub async fn open(
        sqlite_path: impl AsRef<Path>,
        client: qdrant_client::Qdrant,
        collection: impl Into<String>,
        model: DynModel<Embedding>,
        config: StoreConfig,
    ) -> Result<Self, StoreError> {
        config.validate()?;
        let collection = collection.into();
        if collection.trim().is_empty() {
            return Err(StoreError::Configuration(
                "collection must be nonblank".into(),
            ));
        }
        let identity = serde_json::to_string(&(
            2_u32,
            &config.model_identity,
            config.vector_dimension,
            config.max_embedding_bytes,
            &collection,
            model.name(),
            model.id(),
        ))?;
        let storage = storage::Storage::open(sqlite_path, identity).await?;
        let vector =
            qdrant::VectorProjection::open(client, collection, config.vector_dimension).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                storage,
                vector,
                model,
                config,
                projection_gate: Mutex::new(()),
            }),
        })
    }

    /// Returns durable indexing and cleanup progress for the configured scope.
    pub async fn index_status(&self) -> Result<IndexStatus, StoreError> {
        Ok(self.inner.storage.status(&self.inner.config.scope).await?)
    }

    /// Enqueues the Qdrant projection again using the original persisted chunk IDs.
    ///
    /// Does not alter history or perform projection writes. Call `process_pending`
    /// afterward to restore an unavailable or externally deleted index.
    pub async fn rebuild_indexes(&self) -> Result<(), StoreError> {
        let _guard = self.inner.projection_gate.lock().await;
        self.inner.storage.rebuild(&self.inner.config.scope).await?;
        Ok(())
    }
}

impl ConversationMemory for ConversationStore {
    fn load<'a>(
        &'a self,
        id: &'a ConversationId,
    ) -> WasmBoxedFuture<'a, Result<Vec<Message>, MemoryError>> {
        Box::pin(async move {
            self.inner
                .storage
                .load(&self.inner.config.scope, id)
                .await
                .map_err(|error| MemoryError::Backend(Box::new(error)))
        })
    }

    fn append<'a>(
        &'a self,
        id: &'a ConversationId,
        messages: Vec<Message>,
    ) -> WasmBoxedFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move {
            self.inner
                .storage
                .append(
                    &self.inner.config.scope,
                    id,
                    messages,
                    self.inner.config.max_embedding_bytes,
                )
                .await
                .map_err(|error| MemoryError::Backend(Box::new(error)))
        })
    }

    fn clear<'a>(&'a self, id: &'a ConversationId) -> WasmBoxedFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move {
            let _guard = self.inner.projection_gate.lock().await;
            self.inner
                .storage
                .clear(&self.inner.config.scope, id)
                .await
                .map_err(|error| MemoryError::Backend(Box::new(error)))
        })
    }
}

#[cfg(test)]
mod tests;
