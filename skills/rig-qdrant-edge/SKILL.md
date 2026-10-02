---
name: rig-qdrant-edge
description: Add in-process vector retrieval to a native Rust application using rig-qdrant-edge, Rig embedding models, InsertDocuments, and VectorStoreIndex. Use for local shard setup, dense ingestion and search, metadata filters, or connecting an Edge index to a Rig agent.
---

# Embedded Qdrant with Rig

Use `QdrantEdgeVectorStore` behind Rig's existing `InsertDocuments` and
`VectorStoreIndex` traits. This backend stores a local Qdrant Edge shard inside
the Rust process; it requires neither a Qdrant server nor MCP.

## Read the relevant guide

- For dependencies, create/open, persistence, and an offline starter, read
  [references/storage.md](references/storage.md). It uses
  [assets/dense-search.rs](assets/dense-search.rs) and
  [assets/local_embeddings.rs](assets/local_embeddings.rs).
- For embedding selection, document ingestion, filters, or multiple domains,
  read [references/ingestion-and-search.md](references/ingestion-and-search.md).
- For `dynamic_context`, a search tool, custom descriptions, or conversation
  recall, read [references/agent-integration.md](references/agent-integration.md).
  The reusable scoped tool is [assets/search_knowledge.rs](assets/search_knowledge.rs).

Locate the target project's selected Rig revision. Keep Rig companion crates
on one source. The manifest version does not prove that the implementation is
published. Verify signatures in the selected checkout before adapting examples.

The application supplies the embedding model. Use the same model, preprocessing,
vector name, and dimensions for ingestion and query. Matching dimensions alone
do not make embeddings from different models compatible. The local three-axis
model in this skill is an offline fixture, not a production semantic model.

Open a shard directory only once across all processes. Share the store through
clones; the backend does not enforce exclusive directory ownership. Run its
async operations in Tokio. Use explicit `flush()` when persistence failures
must be handled. Never delete or recreate an incompatible shard automatically.

The adapter currently implements dense named Cosine vectors and basic filters.
Do not claim hybrid retrieval, reranking, stable point IDs, delete operations,
or idempotent re-indexing through APIs it does not expose. Document payload IDs
are application references; insertion currently generates new point UUIDs.

Keep parsing and chunking outside storage. Preserve source and canonical
document/conversation references. Enforce trusted scope inside the retrieval
boundary before model-visible results are returned.

Validate offline retrieval with the fixture model and a fresh disposable shard.
Check payload recovery, filters, reopen behavior, and schema errors. Retrieval
correctness with a fixture does not measure real embedding quality.

## Portability and server distinction

Copy this entire directory into a skill directory such as
`~/.codex/skills/rig-qdrant-edge`; invoke `$rig-qdrant-edge`. Guides refer to
source paths inside the selected Rig checkout. Other Rig skills are optional.

`rig-qdrant` is a separate Qdrant server integration using `qdrant-client`.
If the user selects a server, inspect that crate and its example instead.
Do not substitute a server connection for the requested embedded backend.
