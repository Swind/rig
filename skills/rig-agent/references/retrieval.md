# Retrieval in an agent

Use Rig's `VectorStoreIndex` with its backend-specific `Filter`. Data insertion
uses the separate `InsertDocuments` trait and precomputed embeddings. Registering
an index with an agent does not insert documents or index conversations.

## Automatic dynamic context

The builder integration is:

```rust
use rig_agent::AgentBuilder;

// `model` and `index` are configured by the application.
let agent = AgentBuilder::new(model)
    .dynamic_context(5, index)
    .build();
```

This registers retrieval and a completion-call hook. At each applicable model
call boundary, the hook chooses `event.prompt.rag_text()`, or, when absent,
the most recent history message with RAG text. `Message::rag_text()` returns
the first text block of a user message. It does not join the entire dialogue,
summarize it, or use assistant/system text. Missing text skips retrieval.

The hook constructs an unfiltered `VectorSearchRequest` with the selected text
and sample count, calls `top_n::<serde_json::Value>`, and adds the results as
context documents containing the result ID and pretty-printed JSON payload.
Retrieval failure stops the run before the provider call at that boundary.

Read `crates/rig-agent/src/agent/builder.rs` and
`crates/rig-core/src/completion/message.rs` for the authoritative selection logic.
Tests for query selection and hook ordering are in
`crates/rig-agent/src/agent/engine/tests.rs`.

`dynamic_context` has no filter parameter. Use an index adapter that enforces
the host's scope, or a scoped retrieval handler, if data is shared across users
or domains. Registering multiple indexes does not add isolation by itself.
For messaging, the first text block already includes the common metadata header;
the default query therefore includes that header along with the message body.
Use explicit retrieval policy if only the body should be embedded.

## Model-selected search tool

Eligible `VectorStoreIndex` implementations automatically implement
`PortableTool`. The built-in name is `search_vector_store`, with the description
"Retrieves the most relevant documents from a vector store based on a query."
Its advertised arguments are `query`, `samples`, and optional `threshold`;
the advertised schema does not expose a metadata filter. Results contain
`score`, `id`, and `document`. Inspect `crates/rig-core/src/vector_store/mod.rs`.

For a single appropriate index, register `.tool(index)` to let the model decide
when to search and which query to use. The completion model must support tool
calling. This tool performs no automatic writes.

To customize the description, tool name, schema, or mandatory scope, wrap the
index in an application tool implementing `PortableTool` or contextual `Tool`.
Give separate knowledge/history indexes distinct names, such as
`search_knowledge` and `search_past_conversations`; registering several raw
indexes under the same built-in tool name can replace the existing binding.
Resolve tenant/user scope from trusted host configuration or context, not tool
arguments chosen by the LLM.

Choose automatic context when every question should receive retrieved evidence.
Choose a tool when search is conditional, needs a reformulated query, or must
be followed by canonical conversation lookup. Both can coexist, but each adds
its own retrieval cost and policy. Ingestion remains application-owned.

For embedded Qdrant, inspect `crates/rig-qdrant-edge/README.md` and
`crates/rig-qdrant-edge/examples/qdrant_edge_vector_search.rs`. Use the `rig-qdrant-edge` skill when
available for shard ownership, embedding setup, filters, and reusable examples.
The agent-facing contract remains Rig's existing index/tool contract.
