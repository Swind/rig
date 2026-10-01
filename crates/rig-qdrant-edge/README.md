# Rig Qdrant Edge

`rig-qdrant-edge` implements Rig's `InsertDocuments` and `VectorStoreIndex`
traits using Qdrant Edge inside the Rust process. It stores vectors and payloads
in a local shard directory. It does not connect to a Qdrant server.

The application supplies a Rig embedding model. Use the same model and vector
dimensions for insertion and search:

```rust,no_run
use rig_core::vector_store::{InsertDocuments, VectorStoreIndex};
use rig_qdrant_edge::QdrantEdgeVectorStore;

# async fn example(
#     model: impl Into<rig_core::DynModel<rig_core::operation::Embedding>>,
# ) -> Result<(), Box<dyn std::error::Error>> {
let index = QdrantEdgeVectorStore::create("data/knowledge", model, "dense", 384).await?;
let _ = index;
# Ok(())
# }
```

`create` accepts an absent or empty directory. Use `open` to reopen a shard
with the same named Cosine vector and dimension. Both methods check dimensions
reported by the embedding model. The store uses Rig's `EmbeddingsBuilder` and
`InsertDocuments` for writing, and `VectorStoreIndex` for scored or ID-only
search. Payloads must serialize to JSON objects.

Open each directory only once across all processes and share the store through
clones. The backend does not enforce exclusive directory ownership; independently
opening the same directory concurrently can corrupt its storage.

Qdrant Edge operations are synchronous. The adapter runs storage, query,
flush, and optimization calls on Tokio's blocking pool. Call `flush` after an
ingestion batch when the application needs an explicit persistence boundary;
Qdrant Edge 0.8.0 reports flush I/O failures through the adapter's datastore
error. Dropping the last store clone also flushes synchronously and logs I/O
failures, so drop it where blocking is acceptable. Use explicit `flush` when
the application needs to handle persistence errors.

This backend is native-only. Qdrant Edge is distributed under Apache-2.0; Rig's
adapter is distributed under MIT.
