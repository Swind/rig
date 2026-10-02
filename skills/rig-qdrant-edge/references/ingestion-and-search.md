# Embeddings, ingestion, and search

## Index explicitly

Parse and chunk documents in application ingestion code. Serialize each chunk
as a JSON object with text and provenance, such as `source`, `document_id`,
`chunk_index`, and an application namespace. Implement `rig_core::Embed` to
select text for embedding without embedding all metadata accidentally.
The dense-search asset supplies a complete `Chunk` implementation.

Use `EmbeddingsBuilder::new(model.clone()).documents(chunks)?.build().await?`,
then `index.insert_documents(embedded).await?`. `InsertDocuments` accepts
documents paired with precomputed embeddings; it does not parse documents or
choose a provider. The index's model embeds search query strings separately.

Supply the same model and preprocessing throughout. The adapter rejects wrong
widths, non-finite values, values not representable as `f32`, and zero Cosine
vectors. Document payloads must be objects and each document needs at least
one embedding. One point is inserted per embedding with the owning payload.

The current adapter assigns a fresh random point UUID for each insertion.
Payload `document_id` or `message_id` does not become the point ID. Repeating
`insert_documents` can duplicate content; the public adapter has no stable-ID
upsert or deletion API. Do not promise incremental replacement, removal of
old chunks, retention cleanup, or exactly-once indexing. If these are required,
identify the missing capability and implement it deliberately rather than
inventing method names or silently rebuilding live data.

Embedding choice is application-owned. The demonstration fixture requires no
credentials but has no general semantic understanding. For a real model use
the target project's Rig embedding provider or inspect
`crates/rig-fastembed` for local inference. FastEmbed may require model files
and inference runtime downloads; embedded storage alone does not imply a fully
offline embedding setup. Do not add a provider before it is needed.

## Query existing documents

Import `VectorStoreIndex` and `VectorSearchRequest`. Search with
`index.top_n::<Chunk>(request).await?` for `(f64 score, String point_id, Chunk)`
results. `top_n_ids(request)` returns only `(score, point_id)`. The ID is the
backend point reference; canonical provenance remains in the payload.

Requests use `.query(text).samples(n).build()`, optional `.threshold(score)`,
and `.filter(filter)`. Query text must be nonblank and `samples` positive.
The store trims the query before embedding and uses the configured named vector.
Treat returned scores as backend similarity values, not calibrated confidence.
The adapter does not interpret `additional_params` as hybrid search settings.

Filters use `rig_core::vector_store::request::{Filter, SearchFilter}`:

```rust
let filter = Filter::eq("namespace", serde_json::json!("knowledge"));
let request = rig_core::vector_store::VectorSearchRequest::builder()
    .query("deployment guide")
    .samples(5)
    .filter(filter)
    .build();
```

Supported equality values are strings, booleans, and signed integers. Numeric
`Gt`/`Lt` ranges require finite values; `And`/`Or` combine expressions. Unsupported
values return `FilterError` rather than being ignored. Use fields matching the
actual serialized payload. Inspect `crates/rig-qdrant-edge/src/native.rs` before
assuming another filter operator or indexed-payload API exists.

## Separate domains and access scope

Edge exposes shard directories rather than a server database with collections.
Use separate directories for different schemas/models or physical separation.
Alternatively keep compatible data in one shard and require a payload filter
for every scoped search. A `namespace` field alone does not restrict retrieval.

For private conversation data, filter trusted tenant/user/conversation scope
before returning content, and retain canonical storage references. The unfiltered
`dynamic_context` hook is inappropriate for a shared private shard unless the
index/handler itself enforces scope. A custom search tool can enforce host scope
without exposing database filters or authoritative IDs as LLM arguments.

Use `VectorStoreError` variants for embeddings, serialization, request/schema
validation, filters, and storage. Preserve the underlying diagnostic chain in
application logs. Do not return raw datastore diagnostics as tool descriptions
or model-visible content.
