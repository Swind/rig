//! Embedded Qdrant Edge vector storage for Rig.
//!
//! [`QdrantEdgeVectorStore`] implements Rig's document insertion and vector search traits over one
//! persistent local shard. The application supplies the embedding model.
//!
//! ```no_run
//! use rig_core::vector_store::InsertDocuments;
//! use rig_qdrant_edge::QdrantEdgeVectorStore;
//! # async fn example(model: impl Into<rig_core::DynModel<rig_core::operation::Embedding>>) -> Result<(), rig_core::vector_store::VectorStoreError> {
//! let store = QdrantEdgeVectorStore::create("data/knowledge", model, "dense", 384).await?;
//! # let _ = store;
//! # Ok(())
//! # }
//! ```

#[cfg(target_family = "wasm")]
compile_error!("rig-qdrant-edge is a native-only local storage backend");

#[cfg(not(target_family = "wasm"))]
mod native;

#[cfg(not(target_family = "wasm"))]
pub use native::QdrantEdgeVectorStore;
