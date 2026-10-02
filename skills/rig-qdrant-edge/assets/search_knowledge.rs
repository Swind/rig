//! A Rig tool that searches a host-selected namespace in an existing Edge shard.

use rig_core::{
    tool::PortableTool,
    vector_store::{
        VectorSearchRequest, VectorStoreError, VectorStoreIndex, VectorStoreOutput,
        request::{Filter, SearchFilter},
    },
};
use rig_qdrant_edge::QdrantEdgeVectorStore;
use serde::Deserialize;
use serde_json::{Value, json};

/// Search at most five chunks from a host-configured namespace.
pub struct SearchKnowledge {
    /// Clone of the application's single open store for this shard.
    pub index: QdrantEdgeVectorStore,
    /// Mandatory payload scope selected by the host.
    pub namespace: String,
    /// Model-facing description explaining when this knowledge is useful.
    pub description: String,
}

/// Model-supplied query text. The index rejects blank queries.
#[derive(Deserialize)]
pub struct SearchArgs {
    /// Text to embed and search within the configured namespace.
    pub query: String,
}

impl PortableTool for SearchKnowledge {
    const NAME: &'static str = "search_knowledge";
    type Args = SearchArgs;
    type Output = Vec<VectorStoreOutput>;
    type Error = VectorStoreError;

    fn description(&self) -> String {
        self.description.clone()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Text to find relevant knowledge." }
            },
            "required": ["query"]
        })
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let results: Vec<(f64, String, Value)> = self
            .index
            .top_n(
                VectorSearchRequest::builder()
                    .query(args.query)
                    .samples(5)
                    .filter(Filter::eq("namespace", json!(self.namespace)))
                    .build(),
            )
            .await?;
        Ok(results
            .into_iter()
            .map(|(score, id, document)| VectorStoreOutput {
                score,
                id,
                document,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests;
