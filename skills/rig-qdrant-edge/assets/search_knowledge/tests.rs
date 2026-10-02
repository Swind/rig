use super::{SearchArgs, SearchKnowledge};
use rig_core::{
    embeddings::{EmbedError, EmbeddingsBuilder, TextEmbedder},
    tool::PortableTool,
    vector_store::InsertDocuments,
};
use rig_qdrant_edge::QdrantEdgeVectorStore;
use serde::Serialize;

#[path = "../local_embeddings.rs"]
mod local_embeddings;

#[derive(Serialize)]
struct Chunk {
    text: String,
    namespace: String,
    source: String,
}

impl rig_core::Embed for Chunk {
    fn embed(&self, embedder: &mut TextEmbedder) -> Result<(), EmbedError> {
        embedder.embed(self.text.clone());
        Ok(())
    }
}

#[tokio::test]
#[allow(clippy::panic_in_result_fn)]
async fn configured_namespace_excludes_other_matching_documents() -> anyhow::Result<()> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let path = std::env::temp_dir().join(format!("rig-search-tool-{}-{nonce}", std::process::id()));
    let model = local_embeddings::local_embedding_model();
    let index = QdrantEdgeVectorStore::create(&path, model.clone(), "dense", 3).await?;
    let chunks = [
        Chunk {
            text: "qdrant database".into(),
            namespace: "knowledge".into(),
            source: "guide.md".into(),
        },
        Chunk {
            text: "qdrant database".into(),
            namespace: "private".into(),
            source: "private.md".into(),
        },
    ];
    let embedded = EmbeddingsBuilder::new(model)
        .documents(chunks)?
        .build()
        .await?;
    index.insert_documents(embedded).await?;
    let tool = SearchKnowledge {
        index: index.clone(),
        namespace: "knowledge".into(),
        description: "Search project knowledge".into(),
    };
    let hits = tool
        .call(SearchArgs {
            query: "qdrant database".into(),
        })
        .await?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].document["source"], "guide.md");
    drop(tool);
    index.flush().await?;
    drop(index);
    std::fs::remove_dir_all(path)?;
    Ok(())
}
