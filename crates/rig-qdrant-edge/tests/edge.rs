#![cfg(not(target_family = "wasm"))]

use anyhow::{Context, Result, ensure};
use rig_core::{
    embeddings::EmbeddingsBuilder,
    vector_store::{
        InsertDocuments, VectorSearchRequest, VectorStoreIndex,
        request::{Filter, SearchFilter},
    },
};
use rig_qdrant_edge::QdrantEdgeVectorStore;
use serde::{Deserialize, Serialize};

#[path = "../examples/support/local_embeddings.rs"]
mod local_embeddings;

use local_embeddings::{local_embedding_model, local_embedding_model_with_dimensions};

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Chunk {
    text: String,
    source: String,
    namespace: String,
    document_id: String,
    chunk_index: usize,
    user_id: String,
    rating: i64,
    metadata: serde_json::Value,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MultiChunk {
    document_id: String,
    texts: Vec<String>,
    source: String,
}

impl rig_core::Embed for MultiChunk {
    fn embed(
        &self,
        embedder: &mut rig_core::embeddings::TextEmbedder,
    ) -> Result<(), rig_core::embeddings::EmbedError> {
        for text in &self.texts {
            embedder.embed(text.clone());
        }
        Ok(())
    }
}

fn chunk(id: &str, text: &str, user: &str, rating: i64) -> Chunk {
    Chunk {
        text: text.to_owned(),
        source: format!("{id}.md"),
        namespace: "knowledge".to_owned(),
        document_id: id.to_owned(),
        chunk_index: 0,
        user_id: user.to_owned(),
        rating,
        metadata: serde_json::json!({"section": "intro", "labels": ["fixture"]}),
    }
}

async fn indexed_chunks(
    path: &std::path::Path,
    chunks: Vec<Chunk>,
) -> Result<QdrantEdgeVectorStore> {
    let model = local_embedding_model();
    let index = QdrantEdgeVectorStore::create(path, model.clone(), "dense", 3).await?;
    let embedded = EmbeddingsBuilder::new(model)
        .documents(chunks)?
        .build()
        .await?;
    index.insert_documents(embedded).await?;
    Ok(index)
}

fn request(query: &str, samples: u64) -> VectorSearchRequest {
    VectorSearchRequest::builder()
        .query(query)
        .samples(samples)
        .build()
}

fn edge_config(distance: qdrant_edge::Distance) -> qdrant_edge::EdgeConfig {
    let mut config = qdrant_edge::EdgeConfig::default();
    config.vectors.insert(
        "dense".into(),
        qdrant_edge::EdgeVectorParams {
            size: 3,
            distance,
            on_disk: None,
            multivector_config: None,
            datatype: None,
            quantization_config: None,
            hnsw_config: None,
        },
    );
    config
}

#[tokio::test]
async fn flushed_shard_reopens_with_payload_and_search_results() -> Result<()> {
    let temp = assert_fs::TempDir::new()?;
    let path = temp.path().join("knowledge");
    let index = indexed_chunks(
        &path,
        vec![
            chunk("qdrant-guide", "Qdrant database guide", "alice", 5),
            chunk("cooking-guide", "Cooking recipe guide", "alice", 2),
        ],
    )
    .await?;
    index.flush().await?;
    drop(index);

    let reopened = QdrantEdgeVectorStore::open(&path, local_embedding_model(), "dense", 3).await?;
    let results: Vec<(f64, String, Chunk)> = reopened.top_n(request("database", 5)).await?;
    ensure!(results.len() == 2, "expected both persisted chunks");
    let best = results
        .first()
        .context("search returned no persisted chunks")?;
    ensure!(
        best.2.document_id == "qdrant-guide",
        "expected matching chunk first"
    );
    ensure!(
        best.2.source == "qdrant-guide.md",
        "source provenance was lost"
    );
    ensure!(
        best.2.metadata.get("section") == Some(&serde_json::json!("intro")),
        "metadata was lost"
    );
    Ok(())
}

#[tokio::test]
async fn schema_mismatches_and_create_on_existing_data_are_rejected() -> Result<()> {
    let temp = assert_fs::TempDir::new()?;
    let path = temp.path().join("knowledge");
    let index = indexed_chunks(&path, vec![chunk("one", "Qdrant database", "alice", 1)]).await?;
    index.flush().await?;
    drop(index);

    ensure!(
        QdrantEdgeVectorStore::open(&path, local_embedding_model_with_dimensions(4), "dense", 4)
            .await
            .is_err(),
        "opening with an incompatible dimension should fail"
    );
    ensure!(
        QdrantEdgeVectorStore::open(&path, local_embedding_model(), "other", 3)
            .await
            .is_err(),
        "opening with an incompatible vector name should fail"
    );
    ensure!(
        QdrantEdgeVectorStore::create(&path, local_embedding_model(), "dense", 3)
            .await
            .is_err(),
        "create must not overwrite an existing shard"
    );

    let dot_path = temp.path().join("dot-distance");
    std::fs::create_dir_all(&dot_path)?;
    let dot_config = edge_config(qdrant_edge::Distance::Dot);
    drop(qdrant_edge::EdgeShard::new(&dot_path, dot_config)?);
    ensure!(
        QdrantEdgeVectorStore::open(&dot_path, local_embedding_model(), "dense", 3)
            .await
            .is_err(),
        "opening a non-Cosine shard should fail"
    );

    let preserved = QdrantEdgeVectorStore::open(&path, local_embedding_model(), "dense", 3).await?;
    let results: Vec<(f64, String, Chunk)> = preserved.top_n(request("database", 1)).await?;
    ensure!(
        results
            .first()
            .is_some_and(|result| result.2.document_id == "one"),
        "failed schema operations changed stored data"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn flush_reports_storage_failure_and_recovers() -> Result<()> {
    let temp = assert_fs::TempDir::new()?;
    let path = temp.path().join("knowledge");
    let relocated = temp.path().join("relocated");
    let index = indexed_chunks(&path, vec![chunk("one", "Qdrant database", "alice", 1)]).await?;
    std::fs::rename(&path, &relocated)?;
    std::fs::write(&path, b"block shard directory access")?;
    let result = index.flush().await;
    std::fs::remove_file(&path)?;
    std::fs::rename(&relocated, &path)?;
    ensure!(
        matches!(
            result,
            Err(rig_core::vector_store::VectorStoreError::DatastoreError(_))
        ),
        "flush I/O failure should return a datastore error: {result:?}"
    );
    index.flush().await?;
    drop(index);
    let reopened = QdrantEdgeVectorStore::open(&path, local_embedding_model(), "dense", 3).await?;
    ensure!(reopened.top_n_ids(request("database", 1)).await?.len() == 1);
    Ok(())
}

#[tokio::test]
async fn ranked_document_and_id_search_honor_limit_and_filters() -> Result<()> {
    let temp = assert_fs::TempDir::new()?;
    let path = temp.path().join("shared");
    let index = indexed_chunks(
        &path,
        vec![
            chunk("alice-qdrant", "Qdrant database details", "alice", 5),
            chunk("bob-notes", "Miscellaneous notes", "bob", 4),
            chunk("alice-cooking", "Cooking recipe", "alice", 3),
            chunk("alice-misc", "Miscellaneous notes", "alice", 1),
        ],
    )
    .await?;

    let ranked: Vec<(f64, String, Chunk)> = index.top_n(request("database", 2)).await?;
    ensure!(ranked.len() == 2, "sample count should cap results");
    let best = ranked.first().context("search returned no ranked chunks")?;
    let second = ranked
        .get(1)
        .context("search returned fewer than two chunks")?;
    ensure!(best.0 >= second.0, "results should be ranked by score");
    ensure!(
        best.2.document_id == "alice-qdrant",
        "closest result should rank first"
    );
    let ids = index.top_n_ids(request("database", 2)).await?;
    ensure!(ids.len() == 2, "id search should honor sample count");
    ensure!(
        ids.first().map(|result| result.1.as_str()) == Some(best.1.as_str()),
        "document and ID searches should agree"
    );
    let thresholded: Vec<(f64, String, Chunk)> = index
        .top_n(
            VectorSearchRequest::builder()
                .query("database")
                .samples(10)
                .threshold(0.5)
                .build(),
        )
        .await?;
    ensure!(
        thresholded.len() == 1,
        "score threshold should remove less similar vectors"
    );

    let filter = Filter::eq("user_id", serde_json::json!("alice"))
        .and(Filter::gt("rating", serde_json::json!(2)))
        .and(Filter::lt("rating", serde_json::json!(6)));
    let filtered: Vec<(f64, String, Chunk)> = index
        .top_n(
            VectorSearchRequest::builder()
                .query("database")
                .samples(10)
                .filter(filter)
                .build(),
        )
        .await?;
    ensure!(
        filtered.len() == 2,
        "combined user and range filters should match two rows"
    );
    ensure!(
        filtered.iter().all(|result| result.2.user_id == "alice"),
        "user filter leaked"
    );

    let either = Filter::eq("document_id", serde_json::json!("alice-qdrant")).or(Filter::eq(
        "document_id",
        serde_json::json!("alice-cooking"),
    ));
    let selected: Vec<(f64, String, Chunk)> = index
        .top_n(
            VectorSearchRequest::builder()
                .query("database")
                .samples(10)
                .filter(either)
                .build(),
        )
        .await?;
    ensure!(
        selected.len() == 2,
        "OR filter should return either matching document"
    );
    Ok(())
}

#[tokio::test]
async fn separate_shards_isolate_datasets_and_multiple_embeddings_keep_provenance() -> Result<()> {
    let temp = assert_fs::TempDir::new()?;
    let first_path = temp.path().join("first");
    let second_path = temp.path().join("second");
    let _first = indexed_chunks(
        &first_path,
        vec![chunk("first", "Qdrant database", "alice", 1)],
    )
    .await?;
    let second = indexed_chunks(
        &second_path,
        vec![chunk("second", "Qdrant database", "alice", 1)],
    )
    .await?;
    let other: Vec<(f64, String, Chunk)> = second.top_n(request("database", 10)).await?;
    ensure!(
        other.len() == 1
            && other
                .first()
                .is_some_and(|result| result.2.document_id == "second"),
        "datasets mixed records"
    );

    let model = local_embedding_model();
    let path = temp.path().join("multi");
    let multi = QdrantEdgeVectorStore::create(&path, model.clone(), "dense", 3).await?;
    let embedded = EmbeddingsBuilder::new(model)
        .document(MultiChunk {
            document_id: "two-sections".to_owned(),
            texts: vec!["Qdrant database".to_owned(), "Cooking recipe".to_owned()],
            source: "manual.md".to_owned(),
        })?
        .build()
        .await?;
    multi.insert_documents(embedded).await?;
    let results: Vec<(f64, String, MultiChunk)> = multi.top_n(request("database", 10)).await?;
    ensure!(
        results.len() == 2,
        "each embedding should produce one point"
    );
    ensure!(
        results.iter().all(|result| result.2.source == "manual.md"),
        "payload provenance changed"
    );
    let first = results.first().context("first embedded point missing")?;
    let second = results.get(1).context("second embedded point missing")?;
    ensure!(
        first.1 != second.1,
        "generated point IDs should be distinct"
    );
    Ok(())
}

#[tokio::test]
async fn invalid_requests_vectors_embedding_failures_and_storage_errors_are_reported() -> Result<()>
{
    let temp = assert_fs::TempDir::new()?;
    let path = temp.path().join("knowledge");
    let index = indexed_chunks(&path, vec![chunk("one", "Qdrant database", "alice", 1)]).await?;

    ensure!(
        index.top_n::<Chunk>(request("", 1)).await.is_err(),
        "empty query should fail"
    );
    ensure!(
        index.top_n::<Chunk>(request("database", 0)).await.is_err(),
        "zero limit should fail"
    );
    ensure!(
        index
            .top_n::<Chunk>(request("model-error", 1))
            .await
            .is_err(),
        "embedding provider failure should reach the caller"
    );
    ensure!(
        index
            .top_n::<Chunk>(request("wrong-dimension", 1))
            .await
            .is_err(),
        "wrong-width query vectors should fail"
    );
    let model = local_embedding_model();
    let invalid = vec![(
        chunk("bad", "Qdrant database", "alice", 1),
        vec![rig_core::embeddings::Embedding {
            document: "bad".to_owned(),
            vec: vec![1.0, 0.0],
        }],
    )];
    ensure!(
        index.insert_documents(invalid).await.is_err(),
        "wrong-width vectors should fail"
    );
    for vector in [vec![f64::NAN, 0.0, 0.0], vec![f64::MAX, 0.0, 0.0]] {
        let invalid = vec![(
            chunk("bad-float", "Qdrant database", "alice", 1),
            vec![rig_core::embeddings::Embedding {
                document: "bad-float".to_owned(),
                vec: vector,
            }],
        )];
        ensure!(
            index.insert_documents(invalid).await.is_err(),
            "non-finite or out-of-range vector values should fail"
        );
    }
    ensure!(
        index
            .insert_documents(vec![(
                chunk("empty", "Qdrant database", "alice", 1),
                vec![]
            )])
            .await
            .is_err(),
        "documents with no vectors should fail"
    );
    ensure!(
        index
            .insert_documents(vec![(
                "Qdrant database".to_owned(),
                vec![rig_core::embeddings::Embedding {
                    document: "scalar".to_owned(),
                    vec: vec![1.0, 0.0, 0.0],
                }],
            )])
            .await
            .is_err(),
        "scalar payloads should fail"
    );
    let unsupported_filter = Filter::eq("metadata", serde_json::json!(["unsupported"]));
    ensure!(
        index
            .top_n::<Chunk>(
                VectorSearchRequest::builder()
                    .query("database")
                    .samples(1)
                    .filter(unsupported_filter)
                    .build(),
            )
            .await
            .is_err(),
        "unsupported filter value types should fail"
    );
    ensure!(
        QdrantEdgeVectorStore::open(temp.path().join("missing"), model, "dense", 3)
            .await
            .is_err(),
        "opening a missing shard should fail"
    );
    Ok(())
}

#[tokio::test]
async fn explicit_optimization_preserves_search_results() -> Result<()> {
    let temp = assert_fs::TempDir::new()?;
    let path = temp.path().join("knowledge");
    std::fs::create_dir_all(&path)?;
    let mut config = edge_config(qdrant_edge::Distance::Cosine);
    config.optimizers = Some(qdrant_edge::EdgeOptimizersConfig {
        indexing_threshold: Some(1),
        ..Default::default()
    });
    drop(qdrant_edge::EdgeShard::new(&path, config)?);
    let model = local_embedding_model();
    let index = QdrantEdgeVectorStore::open(&path, model.clone(), "dense", 3).await?;
    let documents = EmbeddingsBuilder::new(model)
        .documents((0..300).map(|index| {
            chunk(
                &format!("qdrant-{index}"),
                if index == 0 {
                    "Qdrant database"
                } else {
                    "Cooking recipe"
                },
                "alice",
                1,
            )
        }))?
        .build()
        .await?;
    index.insert_documents(documents).await?;
    let before = index.top_n_ids(request("database", 1)).await?;
    ensure!(
        index.optimize().await?,
        "configured shard should perform optimization"
    );
    let after = index.top_n_ids(request("database", 1)).await?;
    ensure!(before == after, "optimization changed ranked IDs or scores");
    Ok(())
}
