# Connect an Edge index to an agent

Add `rig-agent` from the same source as `rig-core` and `rig-qdrant-edge`.
Configure a completion model separately from the embedding model used by the
index. Ingest documents before testing retrieval-dependent answers.

## Automatic context

Register a clone with `AgentBuilder::new(completion_model)
.dynamic_context(5, index.clone()).build()`. Retrieval runs through a completion
hook, selects the current user prompt's first text block, and falls back to the
most recent history user message with text when the prompt has no text. It
does not search using the full dialogue. Results become context documents with
IDs and serialized payloads. Retrieval errors stop the model call at that boundary.

There is no filter argument on `dynamic_context`. Use a physically separated
shard, an index adapter with mandatory host filters, or a scoped retrieval
handler before applying it to private/shared data. Registering a namespace in
payload does not make automatic retrieval scoped.

## Model-selected tool

The index implements Rig's built-in portable `search_vector_store` tool. A
single `.tool(index.clone())` registers it; its description is generic and its
advertised schema includes `query`, `samples`, and optional `threshold`.
The model decides when to call it and chooses its query. Results contain
`score`, `id`, and the document payload. No documents are automatically inserted.

To provide a meaningful description and domain scope, copy
[../assets/search_knowledge.rs](../assets/search_knowledge.rs) into the consuming
project as `src/search_knowledge.rs`, then declare `mod search_knowledge;`.
Use it during agent construction:

```rust
use rig_agent::AgentBuilder;
use search_knowledge::SearchKnowledge;

// The application creates `completion_model` and opens `index` once.
let search = SearchKnowledge {
    index: index.clone(),
    namespace: "knowledge".into(),
    description: "Search indexed technical knowledge when a question needs project-specific evidence. Results include source provenance.".into(),
};
let agent = AgentBuilder::new(completion_model).tool(search).build();
let answer = agent.prompt("Find the deployment procedure").max_turns(3).await?;
```

This asset implements `PortableTool`, accepts only a query, returns the top five
structured hits, and enforces a configured namespace in every search. The model
does not choose that namespace. Add `serde` with `derive` and `serde_json` as
shown in the storage guide. Change sample count in application code when needed;
the simple schema keeps retrieval implementation settings out of the agent API.

For the bundled offline regression, copy
[../assets/search_knowledge/tests.rs](../assets/search_knowledge/tests.rs) to
`src/search_knowledge/tests.rs` and the fixture
[../assets/local_embeddings.rs](../assets/local_embeddings.rs) to
`src/local_embeddings.rs`. Add `anyhow` and `futures` as development dependencies
if not already present. Run `cargo test configured_namespace_excludes`.
The test creates a disposable local shard with identical matching text in two
namespaces and checks that only the configured namespace's provenance returns.
The `#[cfg(test)] mod tests` declaration expects that sibling test file; copy
both the tool and its test when adopting the template.

For separate history search, use a distinct tool type/name and a description
such as "Search permitted past conversations when the user refers to something
they said earlier; return conversation and message references with excerpts."
Do not register two raw indexes under the same `search_vector_store` name.
Descriptions guide usage but do not enforce access. The asset's namespace
filter is domain separation, not a complete multi-tenant authorization policy.
For private history, add mandatory filters derived from trusted user/tenant
context or use the contextual `Tool` contract.

## Conversation recall and ingestion

Configure `.memory(...)` and stable `.conversation(...)` IDs separately when
normal conversation history should persist. Semantic recall searches selected
indexed chunks, then the application loads the permitted canonical conversation
or message neighborhood using payload references. `ConversationMemory::load`
loads a conversation by ID; arbitrary user/message-window queries belong to
the application's canonical store.

Indexing all conversations is an application ingestion/retention decision.
Neither the agent runtime nor the index integration performs it automatically.
The current Edge adapter's random point IDs and absent deletion API must be
addressed before promising idempotent updates or synchronized retention.

Dense retrieval is the implemented baseline. Future sparse vectors, RRF, and
reranking can be added behind the same Rig index/tool contract when required;
the current adapter exposes no hybrid query API. Do not implement speculative
retrieval layers merely to demonstrate this integration.

Offline acceptance should test the search tool's call directly with the fixture
embedding model and use a mock completion model for agent tool selection and
continuation. Real embedding quality and real model tool behavior require
separate evaluation. Source starting points are
`crates/rig-qdrant-edge/tests/edge.rs` and
`crates/rig-agent/examples/runtime_model_routing.rs`.
