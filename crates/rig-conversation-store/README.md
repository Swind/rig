# rig-conversation-store

A native conversation backend implementing both `ConversationMemory` and
`ConversationSearch`. SQLite stores complete original messages and durable
indexing work and adjacent chunk positions. Qdrant stores keyed embeddings.

Register clones of one backend as agent memory and as
`SearchConversationsTool::new(store.clone())`. Both handles use the configured
access scope. An agent-supplied conversation ID can only narrow this scope.
The `rig` facade exposes this crate as `rig::conversation_store` with the
`conversation-store` feature.

```rust,no_run
use qdrant_client::Qdrant;
use rig_conversation_store::{ConversationStore, StoreConfig};
use rig_core::{DynModel, operation::Embedding, memory::ConversationMemory, completion::Message};

# async fn example(model: DynModel<Embedding>) -> Result<(), Box<dyn std::error::Error>> {
let store = ConversationStore::open(
    "archive.sqlite",
    Qdrant::from_url("http://localhost:6334").build()?,
    "conversation-chunks", model,
    StoreConfig::new("workspace-a", "my-embedding-model-v1", 1536),
).await?;
store.append(&"thread-1".into(), vec![Message::user("Discuss Cypher")]).await?;
store.process_pending(20).await?;
# Ok(()) }
```

Append commits originals and indexing intent in one SQLite transaction. It
does not contact the embedding provider or Qdrant. Applications call
`process_pending(max_jobs)` to make those messages searchable; no worker starts
automatically. `index_status()` reports durable pending work and failures.
After restoring an externally deleted projection, `rebuild_indexes()` marks
active chunks pending again without changing their IDs or original messages.
Retries reuse each persisted chunk UUID. Dropping a processing future leaves
its finite owned task running under the shared clear gate. Await processing
before shutting down Tokio.

One completed append batch becomes a chunk. Partial tool exchanges remain
unindexed until later appends close them. Original messages, including tool and
multimodal content, are never replaced by bounded embedding text. Search uses
the same model to obtain scoped vector seeds, finds the immediately adjacent
chunks in SQLite, then hydrates original excerpts in one SQLite read transaction.
Neighbors must share scope, conversation, and active generation. Qdrant outages
return errors. Complete excerpts that exceed the output
budget are omitted, preserving complete tool exchanges.

Clear waits for the shared gate, advances the SQLite generation, and enqueues projection
cleanup. Stale external references cannot hydrate cleared content. Clear and
indexing are serialized across clones. Use one opened store per scope when
indexing or clearing; separate opens are not a distributed worker lock. Memory
loads retain full history; apply existing memory policies separately.

The embedding identity, dimension, chunking version, text budget, collection,
and model descriptor are persisted for compatibility checks. Changed
index identities require a separate database and re-appending exported original
history to rebuild projections. Query and output budgets can change on reopen.
The Qdrant collection must use one unnamed cosine vector of the configured
dimension. This backend requires SQLite and Qdrant native support.

Run the [shared-memory/search example](examples/conversation_search.rs) with
`OPENAI_API_KEY` and a local Qdrant instance:

```sh
docker run --rm -p 6334:6334 qdrant/qdrant:v1.17.0
cargo run -p rig-conversation-store --example conversation_search
```
