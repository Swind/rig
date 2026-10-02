# Conversation memory and tools

## Conversation memory

Configure `AgentBuilder::memory(backend)` and set a stable conversation ID on
the builder or each `agent.prompt(...).conversation(id)` runner. Rig loads
history before its first model call and appends the successful run's transcript
before returning the response. The transcript includes tool calls and results.

`InMemoryConversationMemory` is a process-local baseline. A durable backend
implements `rig_core::memory::ConversationMemory` with `load`, `append`, and
`clear`. Read `crates/rig-core/src/memory.rs` for signatures, errors, and pointer
forwarding implementations before adding a backend.

A load failure stops before provider calls. An append failure can coexist with
a valid answer and is reported by `PromptResponse.memory_append`. It does not
prove no write occurred. Do not retry the whole agent turn to repair persistence.
Supplying `.history(...)` bypasses both memory loading and appending for that
run; `.without_memory()` disables memory explicitly.

Derive conversation identity from trusted application routing. A user-scoped
chat and a shared channel/thread have different privacy semantics. Direct agent
calls do not provide the per-conversation serialization of `ChatRouter`;
serialize conflicting updates in the application when necessary.

## Portable and contextual tools

`rig_core::tool::PortableTool` defines `NAME`, typed `Args`, typed `Output`,
an error implementing `std::error::Error`, `description`, `parameters`, and
`call(args)`. Rig adapts it to the classic runtime's `Tool` automatically.
Register a tool with `AgentBuilder::tool(tool)`.

When execution needs trusted per-call identity or mutable host state, implement
`rig_agent::tool::Tool`. Its call receives `&mut ToolContext` before the args.
Set host context using `AgentRunner::tool_context(...)`; check the actual
`ToolContext` API before choosing its typed values. Preserve identity across
tool calls without asking the model to supply an authoritative user ID.

`description()` and the JSON schema in `parameters()` form the model-facing
contract. Explain what data the tool searches, when that evidence is useful,
and what its output represents. Keep the schema and argument deserialization
consistent. A custom description influences selection but does not guarantee
that the model calls a tool on every relevant question.

Outputs can be ordinary serializable values, `ToolOutput`, or structured
`ToolResultContent`. Return provenance with retrieved content. The default
error mapping preserves source errors for operators and gives the model safe
kind-level feedback. Expose detailed messages only when they are deliberately
appropriate for the model; never serialize tokens or host-only context.

Inspect `crates/rig-core/src/tool/portable.rs` and `contextual.rs` for current
contracts. `crates/rig-agent/examples/runtime_model_routing.rs` includes a
complete contextual tool round trip with scripted models and no credentials.

## Semantic conversation recall

Maintain the canonical ordered conversation in memory/storage. Index selected
messages or chunks separately, preserving trusted scope, conversation ID,
message ID, and time in the payload. Use semantic retrieval to find references,
then load the permitted conversation/message neighborhood from canonical storage.

`ConversationMemory::load` loads by conversation ID; it is not a user directory
or an arbitrary message-ID query API. If precise message-window lookup is needed,
use the application's canonical store or an appropriate application query API.
Choose what to index and when explicitly. Enforce scope before returning hits,
and propagate deletion/retention decisions to the vector index. A vector index
alone is not a complete conversation archive.
