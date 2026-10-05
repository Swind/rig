# Conversation search contract and tool

## Goal

Let an agent explicitly search saved conversations through a portable tool.
Keep search separate from `ConversationMemory`, which loads and appends the
current conversation's history. A search backend will later coordinate vector
indexes, graph queries, and retrieval of original messages.

This phase implements the public contract, tool wrapper, documentation, and
mock tests. It adds no database implementation, index writer, session store,
automatic prompt injection, or new dependency.

## Public contract

Add `rig_core::conversation_search`, also available as
`rig::conversation_search` through the existing facade re-export.

- `ConversationSearchRequest`: public `query: String`, `limit: u32`, and
  `conversation_id: Option<ConversationId>` fields. Derive serde and use a
  default limit of 5 when JSON omits it. The conversation filter defaults to
  `None`. Keep query required. `validate()` rejects whitespace-only queries
  and limits outside `1..=100`. Do not modify the query text.
- `ConversationSearchHit`: public `conversation_id: ConversationId`,
  `chunk_id: String`, `start_index: u64`, and `messages: Vec<Message>` fields.
  Derive serde. `start_index` is the zero-based position of the first message
  in the saved conversation. Messages are a contiguous original excerpt in
  source order. The backend preserves complete tool-call/result exchanges.
  The chunk ID is a backend-owned stable reference scoped to the conversation.
- `ConversationSearch`: a `WasmCompatSend + WasmCompatSync` trait with
  `search(&self, request: ConversationSearchRequest)` returning a
  WASM-compatible future of `Result<Vec<ConversationSearchHit>,
  ConversationSearchError>`. Use the same return-position future convention
  as `CypherQuery`. Results are ordered by backend relevance, may be empty,
  and must respect the limit and optional conversation filter.
- `ConversationSearchError`: typed `InvalidRequest` and `Backend` variants.
  Validation failures use a descriptive reason. Backend errors retain their
  original boxed source with native/WASM bounds matching `CypherQueryError`.
  Provide a backend-error constructor. Do not use strings as error types.

The backend validates direct requests and owns authorization, ranking, source
hydration, and excerpt size. The tool receives an already scoped backend;
agent-supplied conversation IDs narrow that scope and never expand access.
Neither scores nor graph paths are part of the initial shared result.

## Portable tool

Add `rig_core::tool::builtin::search_conversations::SearchConversationsTool<S>`
and re-export the type from `tool::builtin`. Existing rig-agent and facade
builtin re-exports expose it without new feature flags.

The wrapper has `new(search: S)` and implements `PortableTool` for
`S: ConversationSearch`. Its name is `search_conversations`. Arguments and
output use the shared request and `Vec<ConversationSearchHit>` types.

Its JSON Schema requires only `query`, supplies the same limit default and
bounds as deserialization, and exposes an optional string conversation ID.
The call validates before invoking the backend. It then propagates backend
errors and preserves result order, truncating excess hits to the requested
limit as a final result-count bound. It does not fetch, transform, or inject
messages. Token budgets and concrete backend hydration belong to a later phase.

## Work allocation

1. Contract agent: owns `crates/rig-core/src/conversation_search.rs`, its sibling
   `conversation_search/tests.rs`, and the module declaration in core `lib.rs`.
2. Tool agent: owns `crates/rig-core/src/tool/builtin/search_conversations.rs`,
   its sibling test file, and exports in `tool/builtin/mod.rs`.
3. Integration/docs agent: owns a facade integration test in
   `tests/conversation_search_tool.rs`, root README and core README updates.
   It verifies the public import paths and existing portable-to-contextual
   adaptation using a mock backend, without credentials or model calls.

All agents read this document and existing local APIs before editing. They
leave commits and Cargo execution to the root agent to avoid build locks.
The root reviews every diff, checks the agreed contract, runs focused checks,
and updates this document if acceptance changes the design. The pre-existing
`rig-generic-retrieval-graphrag-design.md` remains untouched.

## Acceptance

Contract tests cover JSON defaults, validation boundaries, source reference
serialization, generic mock calls, and backend error sources. Tool tests cover
schema/default consistency, validation before backend invocation, unchanged
request forwarding, optional scope, ordered output and count bounds, empty
results, error propagation, and conversion to structured tool output.

Run focused core nextest tests, the facade integration target, package-scoped
Clippy, and new module doctests. Compile core for `wasm32-unknown-unknown` with
no default features to verify the public future/error bounds. Review formatting
and documentation links. Do not run the entire workspace or database tests
for a contract-only change. Do not commit without a new explicit request.
