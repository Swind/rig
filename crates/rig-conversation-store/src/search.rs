use std::collections::HashSet;

use rig_core::conversation_search::{
    ConversationSearch, ConversationSearchError, ConversationSearchHit, ConversationSearchRequest,
};

use crate::{ConversationStore, StoreError};

impl ConversationStore {
    async fn search_inner(
        &self,
        request: ConversationSearchRequest,
    ) -> Result<Vec<ConversationSearchHit>, StoreError> {
        if request.query.len() > self.inner.config.max_query_bytes {
            return Err(StoreError::Configuration(
                "search query exceeds the configured byte budget".into(),
            ));
        }
        let vector = self.embedding(&request.query).await?;
        let desired = request.limit as usize;
        let mut seeds = Vec::new();
        let mut seen = HashSet::new();
        let batch = (desired * 2).max(10);
        let mut offset = 0;
        while seeds.len() < desired && offset < self.inner.config.max_candidates {
            let limit = batch.min(self.inner.config.max_candidates - offset);
            let ids = self
                .inner
                .vector
                .query(
                    &self.inner.config.scope,
                    request.conversation_id.as_ref(),
                    vector.clone(),
                    offset,
                    limit,
                )
                .await?;
            let count = ids.len();
            offset += count;
            for chunk in self
                .inner
                .storage
                .active(&self.inner.config.scope, &ids)
                .await?
            {
                if request
                    .conversation_id
                    .as_ref()
                    .is_none_or(|id| id == &chunk.conversation_id)
                    && seen.insert(chunk.id.clone())
                {
                    seeds.push(chunk);
                    if seeds.len() == desired {
                        break;
                    }
                }
            }
            if count < limit {
                break;
            }
        }
        let mut ranked = Vec::new();
        let mut ranked_ids = HashSet::new();
        let mut expanded = 0;
        let seed_ids: HashSet<_> = seeds.iter().map(|seed| seed.id.clone()).collect();
        for seed in seeds {
            if ranked_ids.insert(seed.id.clone()) {
                ranked.push(seed.id.clone());
            }
            let remaining = self
                .inner
                .config
                .max_context_chunks
                .saturating_sub(expanded);
            if remaining == 0 {
                continue;
            }
            let neighbors = self
                .inner
                .storage
                .neighbors(&seed, remaining.min(2))
                .await?;
            expanded += neighbors.len();
            for neighbor in neighbors {
                if !seed_ids.contains(&neighbor) && ranked_ids.insert(neighbor.clone()) {
                    ranked.push(neighbor);
                }
            }
        }
        let mut hits = self
            .inner
            .storage
            .hydrate(
                &self.inner.config.scope,
                &ranked,
                request.conversation_id.as_ref(),
                self.inner.config.max_output_bytes,
            )
            .await?;
        hits.truncate(desired);
        Ok(hits)
    }
}

impl ConversationSearch for ConversationStore {
    async fn search(
        &self,
        request: ConversationSearchRequest,
    ) -> Result<Vec<ConversationSearchHit>, ConversationSearchError> {
        request.validate()?;
        self.search_inner(request)
            .await
            .map_err(ConversationSearchError::backend)
    }
}
