use anyhow::Result;
use rig_core::{
    embeddings::EmbeddingsBuilder,
    vector_store::{
        InsertDocuments, VectorSearchRequest, VectorStoreIndex,
        request::{Filter, SearchFilter},
    },
};
use rig_qdrant_edge::QdrantEdgeVectorStore;
use serde::{Deserialize, Serialize};

mod local_embeddings;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Chunk {
    text: String,
    source: String,
    namespace: String,
    document_id: String,
    chunk_index: usize,
}

impl rig_core::Embed for Chunk {
    fn embed(
        &self,
        embedder: &mut rig_core::embeddings::TextEmbedder,
    ) -> Result<(), rig_core::embeddings::EmbedError> {
        embedder.embed(self.text.clone());
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "data/qdrant-edge-example".to_owned());
    let model = local_embeddings::local_embedding_model();
    let index = QdrantEdgeVectorStore::create(&path, model.clone(), "dense", 3).await?;

    let chunks = [
        Chunk {
            text: "Qdrant stores vectors for fast similarity search.".to_owned(),
            source: "guide.md".to_owned(),
            namespace: "knowledge".to_owned(),
            document_id: "qdrant-guide".to_owned(),
            chunk_index: 0,
        },
        Chunk {
            text: "A cooking recipe combines ingredients and instructions.".to_owned(),
            source: "recipes.md".to_owned(),
            namespace: "knowledge".to_owned(),
            document_id: "recipe".to_owned(),
            chunk_index: 0,
        },
        Chunk {
            text: "A general note about organizing technical references.".to_owned(),
            source: "notes.md".to_owned(),
            namespace: "notes".to_owned(),
            document_id: "notes".to_owned(),
            chunk_index: 0,
        },
    ];
    let embedded = EmbeddingsBuilder::new(model.clone())
        .documents(chunks)?
        .build()
        .await?;
    index.insert_documents(embedded).await?;
    index.flush().await?;
    drop(index);
    let index = QdrantEdgeVectorStore::open(&path, model, "dense", 3).await?;

    let all: Vec<(f64, String, Chunk)> = index
        .top_n(
            VectorSearchRequest::builder()
                .query("qdrant database")
                .samples(5)
                .build(),
        )
        .await?;
    println!("Unfiltered matches:");
    for (score, id, chunk) in all {
        println!("{score:.3}  {id}  {} ({})", chunk.text, chunk.source);
    }

    let knowledge = Filter::eq("namespace", serde_json::json!("knowledge"));
    let filtered: Vec<(f64, String, Chunk)> = index
        .top_n(
            VectorSearchRequest::builder()
                .query("qdrant database")
                .samples(5)
                .filter(knowledge)
                .build(),
        )
        .await?;
    println!("Knowledge matches:");
    for (score, id, chunk) in filtered {
        println!("{score:.3}  {id}  {} ({})", chunk.text, chunk.source);
    }

    Ok(())
}
