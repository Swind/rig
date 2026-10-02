# Shard setup and offline example

This checkout uses `qdrant-edge = "=0.8.0"`, declares Rig 0.42.0, and requires
Rust 1.98.1. Verify the selected checkout rather than relying on a registry
version or a newer upstream API.

## Dependencies and reusable example

For sibling directories `my-search/` and the selected `rig/` checkout:

```toml
[package]
name = "my-search"
version = "0.1.0"
edition = "2024"
rust-version = "1.98.1"

[dependencies]
rig-qdrant-edge = { path = "../rig/crates/rig-qdrant-edge" }
rig-core = { path = "../rig/crates/rig-core", default-features = false }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
futures = "0.3"
anyhow = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Adjust paths to the actual layout. For Git dependencies, pin all Rig crates to
one chosen repository/revision. Use registry dependencies only after checking
that the release includes these APIs. The facade alternative enables
`rig/qdrant-edge` and exports `rig::qdrant_edge::QdrantEdgeVectorStore`.
Edge is native-only; do not enable it in a browser WASM application.

Copy [../assets/dense-search.rs](../assets/dense-search.rs) to `src/main.rs`
and [../assets/local_embeddings.rs](../assets/local_embeddings.rs) beside it
as `src/local_embeddings.rs`. Run `cargo check`, then
`cargo run -- /tmp/my-search-fresh-shard` with an absent or empty directory.
The application writes this directory, creates three example documents,
flushes, drops the store, reopens it, and prints unfiltered and namespace-filtered
results. It does not use provider credentials, a downloaded model, or a server.
Choose another fresh path for a second run because the example deliberately
uses `create` rather than appending to an existing shard.

The files are adapted from
`crates/rig-qdrant-edge/examples/qdrant_edge_vector_search.rs` and
`crates/rig-qdrant-edge/examples/support/local_embeddings.rs`. The fixed three-axis
model tests plumbing and ranks predefined word categories. Replace it with a
real Rig embedding model before assessing semantic retrieval quality.

## Lifecycle contract

`QdrantEdgeVectorStore::create(path, model, "dense", dimensions).await` accepts
an absent or empty directory and creates one named Cosine vector.
`open(path, model, "dense", dimensions).await` validates the persisted named
vector, dimensions, and distance before loading it. Both validate the model's
nonzero reported dimensions. Model changes need explicit re-embedding even
when the width stays the same.

Choose creation or reopening explicitly in application startup. A nonempty
directory is not necessarily a valid shard; surface initialization errors.
Do not reinterpret an `open` failure as permission to delete data.

One independent open owns a directory; clones share the underlying shard.
Avoid separately opening the same path for a tool, ingester, and agent.
Directory exclusivity is the caller's responsibility across processes too.
Storage calls run on Tokio's blocking pool. `flush().await` reports persistence
errors; dropping the last clone flushes synchronously and logs failures.
Do that drop where blocking is acceptable. `optimize().await` exposes Edge
optimization but is not required on every query.

For focused offline checks in the source checkout, inspect
`crates/rig-qdrant-edge/tests/edge.rs`; run the owning crate's integration test
with the repository's local nextest profile. In a consuming project use its
own test configuration. No external embedding API is required for fixture tests.

The separate server adapter is `rig_qdrant::QdrantVectorStore`, with
`QdrantFilter` and collection-configured `QueryPoints`. Its source example is
`crates/rig-qdrant/examples/qdrant_vector_search.rs`. Server collections and
gRPC URLs are not Edge's local directory API.
