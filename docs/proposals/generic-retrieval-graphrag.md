> **Status: historical proposal, not implemented.** This document preserves
> an earlier broad retrieval/GraphRAG design for reference. Its proposed
> `Retriever`, `GraphStore`, `EntityResolver`, and hybrid composition APIs
> are not the current implementation. The implemented boundaries are
> [Cypher queries](../architecture/cypher.md) and
> [conversation search](../architecture/conversation-search.md).

# Proposal: Generic Retrieval and GraphRAG Abstractions for Rig

## 1. Background

Rig currently provides a strong abstraction for vector retrieval through `VectorStoreIndex`.

This works well for conventional RAG pipelines where the primary operation is semantic similarity search:

```text
query
  ↓
embedding
  ↓
vector search
  ↓
top-N documents
  ↓
LLM context
```

However, GraphRAG and other structured retrieval approaches require a different set of operations.

A graph retrieval workflow may need to:

- Resolve entities from a natural-language query
- Traverse relationships from one or more entities
- Restrict traversal by node type or edge type
- Retrieve graph neighborhoods or subgraphs
- Combine graph results with vector search
- Fuse results from multiple retrieval backends
- Rerank the final candidates

Graph traversal is not naturally modeled as a `top_n()` similarity operation.

Therefore, the recommended direction is **not** to introduce a special-purpose `GraphRagStore` that mirrors `VectorStoreIndex`.

Instead, Rig should introduce a more general **retrieval abstraction**, with vector search and graph retrieval implemented as different retrieval strategies.

---

# 2. Design Goals

The proposed design should satisfy the following goals.

## 2.1 Backend independence

Rig core should not depend on:

- SurrealDB
- Neo4j
- Qdrant
- PostgreSQL
- SQLite
- Any specific graph query language

The core API should describe retrieval semantics rather than database implementation details.

---

## 2.2 Preserve existing vector APIs

`VectorStoreIndex` is already useful and should remain supported.

The new retrieval abstraction should be additive rather than immediately replacing the existing API.

A vector index should be adaptable into a generic retriever.

---

## 2.3 GraphRAG should be composition, not storage

GraphRAG is normally a pipeline:

```text
Vector Retrieval
       │
       ├──────────────┐
       │              │
       ▼              ▼
 semantic hits   entity resolution
                      │
                      ▼
                graph traversal
                      │
       ┌──────────────┘
       ▼
     fusion
       │
       ▼
    reranking
       │
       ▼
 RetrievalResult
```

Therefore GraphRAG should be represented as a composition of reusable retrieval components.

---

## 2.4 Support retrieval mechanisms beyond GraphRAG

The abstraction should eventually support:

- Vector search
- BM25 / full-text search
- Graph traversal
- SQL search
- Code search
- Git history search
- Web search
- Time-series retrieval
- MCP-backed retrieval
- Hybrid search
- Custom application retrieval

GraphRAG should be one important use case, rather than the abstraction itself.

---

# 3. Proposed Architecture

The design can be divided into four main abstractions:

```rust
trait Retriever
trait GraphStore
trait EntityResolver
trait Reranker
```

The relationships are roughly:

```text
                   Retriever
                       │
       ┌───────────────┼────────────────┐
       ▼               ▼                ▼
VectorRetriever   GraphRetriever   HybridRetriever
                       │
                       ▼
                GraphRagRetriever
```

At a lower level:

```text
VectorStoreIndex
      │
      ▼
VectorRetriever ─────────────┐
                             │
EntityResolver               │
      │                      │
      ▼                      │
 GraphStore                  │
      │                      │
      ▼                      │
GraphRetriever ──────────────┤
                             ▼
                           Fusion
                             │
                             ▼
                          Reranker
                             │
                             ▼
                       RetrievalResult
```

---

# 4. Generic Retriever

The central abstraction should be a backend-neutral retriever.

Example:

```rust
pub trait Retriever {
    type Error;

    async fn retrieve(
        &self,
        request: RetrievalRequest,
    ) -> Result<RetrievalResult, Self::Error>;
}
```

A minimal request could be:

```rust
pub struct RetrievalRequest {
    pub query: String,
    pub limit: usize,
    pub filters: Option<Filter>,
}
```

The important point is that the request does **not** assume embeddings or graph traversal.

Those are implementation details of a specific retriever.

---

# 5. Retrieval Result

The output should be usable directly by higher-level agent and RAG code.

A practical generic representation is:

```rust
pub struct RetrievalResult {
    pub items: Vec<RetrievedItem>,
}
```

```rust
pub struct RetrievedItem {
    pub id: String,

    /// Textual representation suitable for LLM context.
    pub content: String,

    /// Optional relevance score.
    pub score: Option<f64>,

    pub source: RetrievalSource,

    pub metadata: serde_json::Value,
}
```

For example:

```rust
pub enum RetrievalSource {
    Vector,
    Keyword,
    GraphNode,
    GraphEdge,
    GraphTraversal,
    Sql,
    Web,
    Custom(String),
}
```

This keeps the agent-facing API simple.

The agent usually cares about:

> What relevant information should be added to the context?

It normally does not need to know whether that information came from HNSW, BM25, Cypher, SurrealQL, or a recursive SQL CTE.

---

# 6. Graph Data Model

Graph storage should be modeled independently from GraphRAG.

A minimal generic representation could be:

```rust
pub struct GraphNodeId(pub String);
```

```rust
pub struct GraphNode<T = serde_json::Value> {
    pub id: GraphNodeId,
    pub kind: String,
    pub data: T,
}
```

```rust
pub struct GraphEdge<T = serde_json::Value> {
    pub id: String,
    pub from: GraphNodeId,
    pub to: GraphNodeId,
    pub kind: String,
    pub data: T,
}
```

For example:

```text
runner:runner-03
   │
   │ RUNS_ON
   ▼
server:server-12
   │
   │ HOSTS
   ▼
service:gitlab-runner
```

This representation can map naturally to different storage engines.

### SurrealDB

```text
runner:runner03
    ->runs_on
    ->server:server12
```

### Neo4j

```text
(:Runner)-[:RUNS_ON]->(:Server)
```

### SQLite / PostgreSQL

```text
nodes
edges
```

where an edge table may contain:

```text
from_id
to_id
kind
metadata
```

---

# 7. GraphStore

`GraphStore` should expose graph operations rather than RAG operations.

For example:

```rust
pub trait GraphStore {
    type Node;
    type Edge;
    type Error;

    async fn get_node(
        &self,
        id: &GraphNodeId,
    ) -> Result<Option<Self::Node>, Self::Error>;

    async fn neighbors(
        &self,
        request: GraphTraversalRequest,
    ) -> Result<
        GraphTraversalResult<Self::Node, Self::Edge>,
        Self::Error,
    >;

    async fn search_nodes(
        &self,
        request: GraphNodeSearchRequest,
    ) -> Result<Vec<GraphNodeHit<Self::Node>>, Self::Error>;
}
```

The important design principle is:

> `GraphStore` should model graph semantics, not GraphRAG semantics.

This keeps it reusable outside RAG.

---

# 8. Graph Traversal Request

Graph queries are fundamentally different from vector similarity search.

Typical graph retrieval parameters include:

```rust
pub struct GraphTraversalRequest {
    pub starts: Vec<GraphNodeId>,

    pub direction: Direction,

    pub min_depth: usize,
    pub max_depth: usize,

    pub edge_kinds: Option<Vec<String>>,
    pub node_kinds: Option<Vec<String>>,

    pub max_nodes: usize,
}
```

```rust
pub enum Direction {
    Incoming,
    Outgoing,
    Both,
}
```

For example:

```text
start:
    runner:runner-03

direction:
    outgoing

max_depth:
    2

edge_kinds:
    RUNS_ON
    HOSTS
    DEPENDS_ON
```

This is much more natural than trying to express graph traversal through a `top_n()` method.

---

# 9. Entity Resolution

Graph retrieval normally needs a starting entity.

For a query such as:

```text
Why has runner-03 been failing recently?
```

the system first needs to resolve:

```text
"runner-03"
      ↓
runner:runner-03
```

Therefore entity resolution should be separated from graph traversal.

Proposed API:

```rust
pub trait EntityResolver {
    type Error;

    async fn resolve(
        &self,
        query: &str,
    ) -> Result<Vec<EntityMatch>, Self::Error>;
}
```

```rust
pub struct EntityMatch {
    pub id: GraphNodeId,
    pub score: Option<f64>,
    pub mention: Option<String>,
}
```

An `EntityResolver` may internally use:

- Exact matching
- Full-text search
- BM25
- Vector search
- Aliases
- LLM entity extraction
- Hybrid search

The graph backend should not need to know how an entity was discovered.

---

# 10. GraphRetriever

A graph retriever can compose an entity resolver and graph store.

For example:

```rust
pub struct GraphRetriever<R, G> {
    resolver: R,
    graph: G,
    max_depth: usize,
}
```

Its internal pipeline could be:

```text
query
  ↓
EntityResolver
  ↓
entity IDs
  ↓
GraphStore::neighbors()
  ↓
subgraph
  ↓
RetrievedItem[]
```

This produces a normal `RetrievalResult` that can be consumed by the same agent interfaces as vector retrieval.

---

# 11. VectorRetriever Adapter

Existing vector functionality should remain usable without breaking changes.

An adapter can expose `VectorStoreIndex` through the generic `Retriever` API.

Conceptually:

```rust
pub struct VectorRetriever<I> {
    index: I,
}
```

```rust
impl<I> Retriever for VectorRetriever<I>
where
    I: VectorStoreIndex,
{
    // convert RetrievalRequest
    // into VectorSearchRequest
}
```

This gives the migration path:

```text
VectorStoreIndex
      ↓
VectorRetriever
      ↓
Retriever
```

Existing vector APIs can remain unchanged.

---

# 12. GraphRagRetriever

GraphRAG should be implemented as a composite retriever.

Example:

```rust
pub struct GraphRagRetriever<V, E, G, R> {
    vector: V,
    entity_resolver: E,
    graph: G,
    reranker: Option<R>,
}
```

Conceptual implementation:

```rust
impl<V, E, G, R> Retriever for GraphRagRetriever<V, E, G, R>
where
    V: Retriever,
    E: EntityResolver,
    G: GraphStore,
    R: Reranker,
{
    async fn retrieve(
        &self,
        request: RetrievalRequest,
    ) -> Result<RetrievalResult, Self::Error> {
        // 1. Semantic retrieval
        // 2. Resolve entities
        // 3. Expand graph
        // 4. Normalize candidates
        // 5. Fuse results
        // 6. Optionally rerank
        // 7. Return RetrievalResult
    }
}
```

Typical pipeline:

```text
                    query
                      │
          ┌───────────┴───────────┐
          ▼                       ▼
   VectorRetriever          EntityResolver
          │                       │
          │                       ▼
          │                  entity IDs
          │                       │
          │                       ▼
          │                  GraphStore
          │                       │
          │                       ▼
          │                   subgraph
          │                       │
          └───────────┬───────────┘
                      ▼
                    Fusion
                      │
                      ▼
                   Reranker
                      │
                      ▼
              RetrievalResult
```

---

# 13. Hybrid Retrieval

Once a generic `Retriever` abstraction exists, hybrid search becomes a natural composition.

For example:

```rust
let retriever = HybridRetriever::new()
    .add(vector_retriever, 1.0)
    .add(keyword_retriever, 0.7)
    .add(graph_retriever, 0.8)
    .with_fusion(Rrf::default())
    .with_reranker(reranker);
```

The resulting pipeline:

```text
                    query
                      │
       ┌──────────────┼──────────────┐
       ▼              ▼              ▼
     Vector          BM25           Graph
       │              │              │
       └──────────────┼──────────────┘
                      ▼
                    Fusion
                      │
                      ▼
                   Reranker
                      │
                      ▼
                   Top Results
```

This architecture also allows future retrieval mechanisms to be added without modifying agent code.

---

# 14. Fusion

Multiple retrievers may return scores that are not directly comparable.

For example:

```text
Vector:
    cosine similarity

BM25:
    BM25 score

Graph:
    graph distance / relationship weight

SQL:
    potentially no relevance score
```

Therefore the hybrid layer should not assume that raw scores can simply be added.

Useful fusion strategies include:

- Reciprocal Rank Fusion
- Weighted Reciprocal Rank Fusion
- Rank normalization
- Score normalization
- Reranker-only fusion

A possible abstraction:

```rust
pub trait FusionStrategy {
    fn fuse(
        &self,
        results: Vec<RetrievalResult>,
    ) -> RetrievalResult;
}
```

RRF is a particularly useful default because it operates on ranking rather than requiring comparable score scales.

---

# 15. Reranking

Reranking should remain independent from the retrieval backend.

Possible abstraction:

```rust
pub trait Reranker {
    type Error;

    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RetrievedItem>,
        limit: usize,
    ) -> Result<Vec<RetrievedItem>, Self::Error>;
}
```

Possible implementations include:

- Cross-encoder reranker
- LLM reranker
- Provider-based reranking API
- Local reranking model
- No-op reranker

This allows the same reranker to operate over mixed results from:

- Vector search
- BM25
- Graph traversal
- SQL
- External search tools

---

# 16. Suggested `rig-core` Module Layout

A possible organization:

```text
rig-core/
├── retrieval/
│   ├── mod.rs
│   ├── retriever.rs
│   ├── request.rs
│   ├── result.rs
│   ├── source.rs
│   ├── fusion.rs
│   └── reranker.rs
│
├── graph/
│   ├── mod.rs
│   ├── store.rs
│   ├── node.rs
│   ├── edge.rs
│   ├── traversal.rs
│   └── entity.rs
│
└── vector_store/
    └── existing implementation
```

An alternative is to place graph-specific abstractions underneath retrieval:

```text
retrieval/
├── graph/
├── vector/
├── hybrid/
└── ...
```

The choice mainly depends on whether graph storage is expected to be useful outside retrieval.

If Rig intends to expose graph data as a general storage primitive, keeping `graph/` separate is cleaner.

---

# 17. Database Integration Crates

Backend-specific implementations should remain outside `rig-core`.

For example:

```text
rig-surrealdb
├── SurrealVectorStore
├── SurrealGraphStore
└── SurrealEntityResolver

rig-neo4j
├── Neo4jVectorIndex
├── Neo4jGraphStore
└── Neo4jEntityResolver

rig-qdrant
└── QdrantVectorStore

rig-sqlite
├── SqliteGraphStore
└── SqliteKeywordRetriever
```

This avoids introducing database-specific types into the core retrieval API.

---

# 18. SurrealDB as a Useful First Graph Backend

SurrealDB is particularly interesting for this design because it can provide:

- Structured records
- Graph relations
- Full-text search
- Vector search
- Hybrid retrieval

That allows a single database to implement several components:

```text
SurrealDB
├── VectorRetriever
├── KeywordRetriever
├── EntityResolver
└── GraphStore
```

A GraphRAG pipeline could therefore use one physical database while still preserving clean logical abstractions.

For example:

```text
                SurrealDB
                   │
       ┌───────────┼───────────┐
       ▼           ▼           ▼
     Vector       BM25        Graph
       │           │           │
       └───────────┼───────────┘
                   ▼
                 Fusion
                   │
                   ▼
                Reranker
```

The important point is that Rig should not assume all of these capabilities come from the same database.

A user may instead choose:

```text
Qdrant
   +
PostgreSQL
   +
Neo4j
```

and still use exactly the same high-level retrieval APIs.

---

# 19. Example: Infrastructure Agent

Assume the knowledge graph contains:

```text
runner:runner-03
    │
    ├──RUNS_ON──────────> server:server-12
    │
    └──BELONGS_TO───────> project:chromium-ci

server:server-12
    │
    ├──MOUNTS───────────> volume:data
    │
    └──HAS_ALERT────────> alert:disk-full
```

User query:

```text
Why has runner-03 been failing recently?
```

The retrieval pipeline can execute:

### Step 1: Entity resolution

```text
runner-03
    ↓
runner:runner-03
```

### Step 2: Graph expansion

```text
runner:runner-03
    ├── RUNS_ON → server:server-12
    └── BELONGS_TO → project:chromium-ci

server:server-12
    ├── MOUNTS → volume:data
    └── HAS_ALERT → alert:disk-full
```

### Step 3: Semantic search

Use the server, project, runner, and alert identifiers as additional semantic search terms.

Retrieve log fragments such as:

```text
/data reached 98% usage
```

### Step 4: Fusion

Combine:

- Graph facts
- Relevant logs
- Incident documents

### Step 5: Rerank

Select the highest-value evidence.

### Step 6: Agent context

Provide a compact representation:

```text
Runner runner-03 runs on server-12.
server-12 has a disk-full alert.
The /data volume reached 98% usage.
Recent runner failures correlate with insufficient disk space.
```

This is significantly more robust than using vector similarity alone.

---

# 20. Agent Integration

The agent-facing API should remain simple.

Current vector-oriented usage may look conceptually like:

```rust
agent.dynamic_context(4, vector_index)
```

A future API might become:

```rust
agent.retriever(
    VectorRetriever::new(vector_index),
);
```

or:

```rust
agent.retriever(
    GraphRagRetriever::builder()
        .vector(vector_retriever)
        .entity_resolver(entity_resolver)
        .graph(graph_store)
        .reranker(reranker)
        .build(),
);
```

The agent should not need to understand how retrieval is implemented.

---

# 21. Tool-Based Retrieval

Rig already has a natural relationship between agents and tools.

Graph retrieval can optionally also be exposed as explicit tools.

For example:

```text
search_knowledge(query)

find_entity(name)

expand_entity(
    entity_id,
    depth,
    relation_types
)

find_path(
    from,
    to
)
```

There are therefore two valid modes.

## Automatic retrieval

```text
User
  ↓
Retriever
  ↓
context injection
  ↓
Agent
```

## Agent-controlled retrieval

```text
User
  ↓
Agent
  ↓
Graph search tool
  ↓
GraphStore
  ↓
Agent
```

Both should be supported.

The underlying `GraphStore` and `Retriever` abstractions can be shared between them.

---

# 22. Avoid Exposing Query Languages in Core

Rig core should avoid APIs such as:

```rust
graph.cypher("MATCH ...")
```

or:

```rust
graph.surrealql("SELECT ...")
```

as the primary graph abstraction.

Backend-specific crates may expose native escape hatches, but generic agent code should operate through graph semantics:

```rust
GraphTraversalRequest {
    starts,
    direction,
    depth,
    edge_kinds,
    ...
}
```

This preserves portability.

---

# 23. Typed vs Dynamic Graph Data

A practical initial implementation should probably use dynamic payloads:

```rust
serde_json::Value
```

for graph node and edge metadata.

This keeps the trait usable across arbitrary domains.

Applications can layer typed wrappers on top.

For example:

```rust
#[derive(Deserialize)]
struct Runner {
    hostname: String,
    executor: String,
}
```

The core graph API should not require a single global schema.

---

# 24. Graph Retrieval Safety

Graph expansion must always be bounded.

A graph can grow exponentially:

```text
depth 1 → 10 nodes
depth 2 → 100 nodes
depth 3 → 1,000 nodes
depth 4 → 10,000 nodes
```

Therefore traversal requests should support hard limits such as:

```rust
max_depth
max_nodes
edge_kinds
node_kinds
```

A reasonable GraphRAG default is often:

```text
depth: 1–2

occasionally:
depth: 3
```

rather than unrestricted traversal.

---

# 25. Ranking Graph Results

Graph retrieval does not inherently have a vector-style similarity score.

Possible graph relevance signals include:

- Graph distance
- Entity resolution score
- Edge weight
- Relationship type importance
- Node centrality
- Recency
- Number of matching paths
- Semantic similarity of node descriptions

A practical GraphRetriever could calculate:

```text
graph_score =
    entity_score
    × distance_decay
    × relation_weight
```

However, the generic `Retriever` interface should not enforce a particular graph ranking algorithm.

---

# 26. Recommended Initial Scope

The first implementation should stay small.

Recommended Phase 1:

```text
Retriever
RetrievalRequest
RetrievalResult
RetrievedItem

GraphStore
GraphNode
GraphEdge
GraphTraversalRequest

EntityResolver

VectorRetriever adapter
GraphRetriever
```

Do not initially implement every possible GraphRAG algorithm.

---

# 27. Phase 2

Add:

```text
HybridRetriever
RRF
Reranker
parallel retrieval
result deduplication
```

This enables:

```text
Vector + BM25 + Graph
```

pipelines.

---

# 28. Phase 3

Add advanced graph retrieval:

```text
path search
weighted traversal
graph ranking
community retrieval
subgraph summarization
temporal relationships
graph-aware reranking
```

These should be added only after practical use cases demonstrate that they are needed.

---

# 29. Compatibility Strategy

Avoid immediately deprecating:

```rust
VectorStoreIndex
```

Instead:

```text
existing API
    │
    ├── continues working
    │
    └── can be adapted
            ↓
        VectorRetriever
            ↓
         Retriever
```

This avoids a large breaking change.

Users that only need vector RAG do not need to understand the new graph APIs.

---

# 30. Suggested Upstream Positioning

If this is proposed upstream to Rig, it is better framed as:

> Generic Retrieval Abstraction

rather than:

> Add GraphRAG support

GraphRAG is a strong motivating use case, but the architectural gap is broader.

The missing abstraction is:

```text
Agent
   ↓
Retriever
   ↓
arbitrary information retrieval mechanism
```

instead of the current assumption:

```text
Agent
   ↓
VectorStoreIndex
   ↓
vector similarity
```

This makes the feature broadly useful for future retrieval systems.

---

# 31. Recommended Core Traits

The proposal can be summarized with four main traits:

```rust
pub trait Retriever {
    async fn retrieve(...) -> RetrievalResult;
}
```

```rust
pub trait GraphStore {
    async fn get_node(...);
    async fn neighbors(...);
}
```

```rust
pub trait EntityResolver {
    async fn resolve(...);
}
```

```rust
pub trait Reranker {
    async fn rerank(...);
}
```

And the composition becomes:

```text
                    Retriever
                        │
       ┌────────────────┼────────────────┐
       ▼                ▼                ▼
VectorRetriever    GraphRetriever   OtherRetriever
                        │
                        ▼
                 GraphRagRetriever
                        │
                        ▼
                  HybridRetriever
                        │
                        ▼
                     Agent
```

---

# 32. Final Recommendation

For Rig, the recommended architecture is:

1. Keep `VectorStoreIndex`.
2. Introduce a generic `Retriever`.
3. Add `VectorRetriever` as an adapter around existing vector indexes.
4. Introduce a low-level backend-neutral `GraphStore`.
5. Keep entity resolution separate through `EntityResolver`.
6. Implement `GraphRetriever` as a composition of resolver + graph store.
7. Implement GraphRAG as a higher-level composite retriever.
8. Add fusion and reranking independently.
9. Keep database-specific implementations in integration crates.
10. Use SurrealDB as a useful initial GraphRAG backend, but never design the core API specifically around SurrealDB.

The resulting architecture gives Rig a reusable foundation for:

```text
Vector RAG
GraphRAG
Hybrid RAG
BM25
SQL retrieval
code retrieval
web retrieval
custom enterprise retrieval
```

without coupling the agent framework to any specific storage engine or retrieval strategy.
