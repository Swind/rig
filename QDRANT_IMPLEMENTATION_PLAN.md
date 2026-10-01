# Rig and Qdrant Edge Integration Plan

## Goal and scope

Implement an embedded Qdrant Edge backend for Rig's existing embedding and
vector-store interfaces. Storage and retrieval run inside the Rust application
process and persist data in local directories. The first milestone covers
initialization, insertion, dense search, payload filters, provenance, and reopening
stored data.

Use the `qdrant-edge` Rust library rather than `qdrant-client`. No Qdrant server,
gRPC connection, Docker container, MCP server, or HTTP retrieval service is needed.
Do not add `KnowledgeStore` or `KnowledgeService`. Agent tools, automatic
conversation ingestion, parsers, chunkers, hybrid search, and rerankers remain
subsequent work.

```text
Rust application process
├── Rig embedding model
├── Rig EmbeddingsBuilder
└── Qdrant Edge adapter
    ├── InsertDocuments
    ├── VectorStoreIndex
    └── shared EdgeShard → local storage directory
```

Embedding inference is a separate dependency. The application chooses the model;
Edge receives vectors rather than choosing a dense embedding model. An embedded
database does not imply local inference. Tests use a deterministic local model.
A fully offline application also needs locally available model files and an
in-process inference implementation.

## Existing interfaces and implementation boundary

Reuse these verified Rig interfaces:

| Capability | Interface | Source |
|---|---|---|
| Embedding model | `Model`, `DynModel<operation::Embedding>` | [embedding.rs](crates/rig-core/src/embeddings/embedding.rs) |
| Document embedding | `Embed`, `EmbeddingsBuilder` | [builder.rs](crates/rig-core/src/embeddings/builder.rs) |
| Insertion | `InsertDocuments::insert_documents()` | [vector_store/mod.rs](crates/rig-core/src/vector_store/mod.rs) |
| Search | `VectorStoreIndex::top_n()`, `top_n_ids()` | [vector_store/mod.rs](crates/rig-core/src/vector_store/mod.rs) |
| Requests and filters | `VectorSearchRequest`, `Filter`, `SearchFilter` | [request.rs](crates/rig-core/src/vector_store/request.rs) |
| Local embedding integration | `Local<operation::Embedding>` | [local.rs](crates/rig-core/src/driver/local.rs) |

The existing [rig-qdrant implementation](crates/rig-qdrant/src/lib.rs) is a
server-backed adapter. Its `QdrantVectorStore`, `QueryPoints`, and gRPC filter
conversion are not an Edge implementation.

Add a companion crate, provisionally named `rig-qdrant-edge`, rather than
replacing `rig-qdrant` or changing Rig core traits. The proposed adapter name is
`QdrantEdgeVectorStore`. These names describe planned additions, not existing APIs.

When exposing the new companion crate, update the workspace dependency, root
optional dependency, facade feature and re-export, example, root README, companion
README, and crate docs. The facade names are feature `qdrant-edge` and module
`rig::qdrant_edge`. The facade dependency and re-export are native-only so the
facade's WASM all-features build remains available.

## Phase 0: verify the Edge dependency

Before implementing the adapter:

1. Pin `qdrant-edge = "=0.6.1"` in the workspace dependency table and use it
   from the crate manifest. This version passes the stable compiler probe and
   shares `geo` 0.32 with SurrealDB. Version 0.7.2 conflicts with the workspace
   `i_overlay` resolution; 0.8.0 uses unstable `std::debug_assert_matches`.
   Root verification confirms the selected dependency graph on Linux with Rust
   1.95 before implementation is accepted.
2. Document native-only support. The Edge crate uses filesystem, C++ SIMD, and
   process libraries and is exposed by Rig only on native targets.
3. Check shard creation, loading, configuration inspection, updates, queries,
   flushing, optimization, and ownership requirements.
4. Confirm thread safety and synchronous call behavior before selecting shared
   ownership or any blocking execution mechanism.
5. Inspect existing companion crate conventions and the closest vector-store
   implementation before adding files.

Do not add guessed Edge API signatures or copy server request structs into the
new adapter. If a dependency constraint prevents the intended integration, report
that constraint before implementing a substitute backend.

## Dataset organization

Edge uses `EdgeShard` rather than Server collections. Each dataset can have its
own shard and directory. Payload fields can partition records within a shard.
These differences are documented in the
[official Edge and Server comparison](https://qdrant.tech/documentation/edge/edge-vs-qdrant-cluster/).

```text
Application
├── conversation_index → EdgeShard → data/conversations/
└── knowledge_index    → EdgeShard → data/knowledge/
```

Each adapter instance searches its configured shard. Keep one shared shard handle
per opened dataset rather than reopening a directory for every request. Open each
directory only once across all processes and share the adapter through clones.
Qdrant Edge 0.6.1 does not enforce exclusive directory ownership, so concurrent
independent opens are unsupported. Multiple users can share the conversation shard,
with an application-enforced `user_id`
filter. Knowledge topics can share the knowledge shard with `domain` or `source`
filters. Do not create a shard for every message or conversation.

The milestone tests two independent datasets to prove that queries do not mix
records across shards. The example can use knowledge chunks; conversation memory
integration is a later consumer of the same adapter.

## Initialization and persistence

The adapter holds the embedding model, a shared shard handle, and the selected
vector name. Use one named dense vector, `dense`, with Cosine distance from the
start. The new writer and reader both target that name; the old server writer's
unnamed-vector limitation does not apply to a new Edge adapter.

Separate creation from opening existing data. Create only in an empty location;
load an existing shard using the selected release's loading API. Never interpret
an arbitrary load error as permission to create or overwrite data. The official
[quickstart](https://qdrant.tech/documentation/edge/edge-quickstart/) distinguishes
creating a shard from loading an existing one.

At startup:

1. Construct the embedding model and determine expected vector dimensions.
   Use valid `model.capabilities().ndims`, or require and validate an explicit
   dimension setting when the model does not provide one.
2. Create a missing dataset with `dense`, the expected dimensions, and Cosine;
   otherwise open and inspect the stored schema.
3. Reject incompatible dimensions, vector names, or distance metrics with a clear
   error. Preserve the existing data.
4. Confirm that the application uses the same embedding model and settings for
   ingestion and queries. Equal dimensions alone do not establish compatibility.
5. Construct the adapter using the opened shard and model.

For `qdrant-edge` 0.6.1, `flush()` returns unit and can panic on lock or I/O
failures. Run synchronous storage operations off the async executor; convert a
panic from the blocking task into the datastore error. Verify successful writes
survive an explicit flush, release of all handles, and reopening the shard. This
establishes only successful explicit-flush persistence, not crash durability or
multi-process access. Changing embedding models requires re-embedding or a
separate dataset.

## Insertion and payloads

The application provides documents implementing `Embed` and `Serialize`.
`EmbeddingsBuilder` generates `(document, embeddings)` pairs. The adapter's
`InsertDocuments` implementation serializes payloads, converts vectors to the
backend representation, and writes points into the configured dense vector.

For the example, each input is one previously prepared chunk with only `text`
embedded. Preserve `text`, `source`, `namespace`, `document_id`, `chunk_index`, and
an object-valued `metadata` payload. Conversation applications can instead store
`user_id`, `conversation_id`, `message_id`, timestamp, role, and text. These are
application payload fields, not required fields of the generic adapter.

Define and test how multiple embeddings per document become separate points with
the same payload. Reject empty or incorrectly sized embeddings before writing.
Use Rig's existing embedding batching and the smallest useful Edge update batch.
Document actual partial-write behavior; do not promise dataset-wide atomicity.

Rig's generic insertion trait does not supply caller-selected point IDs. For this
milestone, generated UUIDs identify new points, and the example demonstrates a
fresh dataset rather than repeated re-indexing. Do not infer an ID from an
arbitrary payload field. Stable-ID upserts and deletion are subsequent
backend-specific ingestion operations; they do not require replacing Rig's search
interface.

## Search and filters

Implement both `VectorStoreIndex` methods:

- `top_n::<T>()`: return `(f64 score, String point_id, T payload)`.
- `top_n_ids()`: return `(f64 score, String point_id)` without fetching payloads
  unnecessarily.

Use Rig's existing `Filter<serde_json::Value>` as the initial filter type and
translate it into the selected Edge release's native conditions inside the
adapter. It already expresses equality, ranges, AND, and OR. Verify backend value
types and return a filter error for unsupported input. This avoids depending on
the server crate solely for its gRPC filter conversion.

For every search:

1. Validate a nonempty query and positive sample count.
2. Embed the query with the configured Rig model and validate its dimensions.
3. Translate the request filter and query the shard's `dense` vector with the
   requested limit and optional score threshold.
4. Recover point IDs and deserialize payloads for scored-document searches.
5. Map failures into existing `VectorStoreError` variants and preserve sources.

The example defaults to five results. Score thresholds follow the configured
backend metric and are not confidence probabilities. Apply filters inside the
backend query rather than filtering a global top-N result afterward. Applications
must add authorization conditions, such as the current `user_id`, to every
applicable request independently of model-supplied arguments.

## Blocking work and optimization

Edge exposes synchronous local operations. Inspect the selected release and use
the repository's execution conventions to keep substantial disk or indexing work
from blocking the agent's async executor. Choose only the runtime support needed
for this implementation; do not introduce a general scheduler or worker framework.

Unlike Server's background optimization, Edge indexing and optimization are
explicit. The application owns when to optimize, such as after an ingestion batch.
New points remain searchable before optimization. Verify equivalent retrieval
behavior before and after optimization. Avoid calling `optimize()` for every query
or inserted chunk. See the
[official operations comparison](https://qdrant.tech/documentation/edge/edge-vs-qdrant-cluster/).

## Implementation phases and tasks

### Phase 1: implement the adapter

**1.1 Lifecycle and schema.** Add create/open operations, shared shard
ownership, schema validation, flush, reopening, and explicit optimization.
Use only `qdrant-edge` 0.6.1 APIs and document native target support.

**1.2 Insertion, filters, and search.** Implement `InsertDocuments` and both
`VectorStoreIndex` methods. Preserve payload provenance, translate equality,
range, AND, and OR filters, validate inputs, and map errors to Rig's
`VectorStoreError` variants. Use the existing WASM-compatible bounds where
applicable.

**1.3 Blocking work and errors.** Keep synchronous disk and indexing work off
the async executor. Convert blocking-task panics, including 0.6.1 flush panics,
into datastore errors. Add sibling-file unit tests where needed.

### Phase 2: add local tests and the example

**2.1 Deterministic helper.** Provide a small local embedding transport shared
by tests and the example. It must not download models or require credentials.

**2.2 Offline suite.** Use temporary shard directories. Cover create/open and
explicit-flush persistence, schema rejection without data loss, embedding and
insertion, ranked and ID-only search, payload provenance, multiple embeddings,
filters, dataset isolation, invalid inputs and errors, and retrieval before and
after optimization. Use distinct vector directions to verify Cosine ranking.

**2.3 Example.** Add `qdrant_edge_vector_search` to create a fresh local shard,
embed and insert prepared chunks, and search with and without a filter. Print
scores, point IDs, and provenance; explain that rerunning insertion uses new
random point IDs.

### Phase 3: expose and document the integration

**3.1 Facade wiring.** Add the optional native-target companion dependency,
`qdrant-edge` feature, `rig::qdrant_edge` re-export, docs.rs feature, and facade
feature-fixture entry. Preserve WASM facade all-features support.

**3.2 Documentation and CI.** Add root README and test command coverage, crate
documentation for local shards and directories, and the native-only diagnostic
check in xtask and CI. Keep the existing server-backed `rig-qdrant` behavior
unchanged.

### Phase 4: accept and review

**4.1 Focused checks.** Run the package tests, formatting and lint checks, and
the facade feature compile:

```sh
cargo check --locked -p rig-qdrant-edge --all-targets
cargo clippy --locked -p rig-qdrant-edge --all-targets
cargo nextest run --locked --profile local -p rig-qdrant-edge
cargo check --locked -p rig --no-default-features --features qdrant-edge
```

Adjust commands to the actual manifest. Leave workspace-wide, facade
all-features, and target matrices to CI.

**4.2 Independent review.** Review the complete diff for API correctness,
error handling, persistence claims, filter conversion, native-only wiring, and
documentation consistency. Follow [DEVELOPING.md](DEVELOPING.md) and
[tests/README.md](tests/README.md). This planning document does not claim
implementation or checks are complete.

## Acceptance criteria

A Rust application uses Rig's embedding abstraction and vector-store traits to
insert and retrieve documents through an in-process Qdrant Edge shard. Queries
return correct ranking, filters, IDs, and provenance. Stored data remains
searchable after reopening, and separate datasets do not mix. Local integration
tests require neither a server nor external embedding credentials.

## Subsequent work

The adapter can later support `dynamic_context()` and tools through Rig's existing
integration paths. Separate conversation and knowledge tools need distinct names
and descriptions; directly registering two indexes uses the same built-in
`search_vector_store` name. Wrappers continue to call `VectorStoreIndex`.

Conversation memory integration adds explicit message ingestion, stable-ID
upserts, deletion synchronization, and lookup of surrounding messages in the
original conversation store. Rig does not automatically persist conversations to
the vector index. Stable point IDs do not remove surplus chunks by themselves;
define replacement semantics when implementing re-indexing.

Sparse vectors, fusion, reranking, server synchronization, and schema migrations
remain future work. A named dense vector establishes the initial schema without
requiring those features now.

## Official references

- [Qdrant Edge overview](https://qdrant.tech/documentation/edge/)
- [Edge API reference](https://qdrant.tech/documentation/edge/edge-api/)
- [On-device embeddings](https://qdrant.tech/documentation/edge/edge-fastembed-embeddings/)

The implementation must follow the selected Rust release's source and signatures,
including any differences from the documentation's Python examples.
