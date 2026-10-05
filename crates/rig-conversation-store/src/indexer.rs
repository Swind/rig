use crate::{ConversationStore, IndexStatus, StoreError, storage::Chunk};

impl ConversationStore {
    /// Processes at most `max_jobs` durable indexing or cleanup jobs.
    ///
    /// Each acknowledged projection is committed to SQLite. On failure the
    /// current job remains retryable, its failure is recorded, and the error is
    /// returned. Dropping this future does not cancel its finite owned task;
    /// that task keeps the shared clear gate until projection acknowledgment.
    /// Await processing before shutting down the Tokio runtime.
    pub async fn process_pending(&self, max_jobs: usize) -> Result<IndexStatus, StoreError> {
        if max_jobs > 10000 {
            return Err(StoreError::Configuration(
                "max_jobs cannot exceed 10000".into(),
            ));
        }
        let store = self.clone();
        tokio::spawn(async move { store.process_batch(max_jobs).await }).await?
    }

    async fn process_batch(&self, max_jobs: usize) -> Result<IndexStatus, StoreError> {
        let _guard = self.inner.projection_gate.lock().await;
        let jobs = self
            .inner
            .storage
            .pending(&self.inner.config.scope, max_jobs)
            .await?;
        for chunk in jobs {
            if let Err(error) = self.project(&chunk).await {
                self.inner
                    .storage
                    .record_failure(&chunk.id, &error.to_string())
                    .await?;
                return Err(error);
            }
        }
        self.index_status().await
    }

    async fn project(&self, chunk: &Chunk) -> Result<(), StoreError> {
        if !chunk.retired
            && self
                .inner
                .storage
                .active(&chunk.scope, std::slice::from_ref(&chunk.id))
                .await?
                .is_empty()
        {
            return Ok(());
        }
        if !chunk.vector_done {
            if chunk.retired {
                self.inner.vector.delete(chunk).await?;
            } else {
                let vector = self.embedding(&chunk.text).await?;
                self.inner.vector.upsert(chunk, vector).await?;
            }
            self.inner
                .storage
                .mark_projection(&chunk.id, chunk.retired)
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn embedding(&self, text: &str) -> Result<Vec<f32>, StoreError> {
        let embedding = self.inner.model.embed_text(text).await?;
        if embedding.vec.len() as u64 != self.inner.config.vector_dimension {
            return Err(StoreError::Configuration(
                "embedding dimension does not match persisted configuration".into(),
            ));
        }
        embedding
            .vec
            .into_iter()
            .map(|value| {
                let narrowed = value as f32;
                if narrowed.is_finite() {
                    Ok(narrowed)
                } else {
                    Err(StoreError::Configuration(
                        "embedding contains a non-finite or overflowing component".into(),
                    ))
                }
            })
            .collect()
    }
}
