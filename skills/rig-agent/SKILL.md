---
name: rig-agent
description: Build or extend a Rust application using Rig's AgentBuilder and AgentRunner, including model configuration, tool calls, conversation memory, and retrieval context. Use when integrating the rig-agent runtime or adapting existing Rig agent examples.
---

# Rig agent integration

Use the existing Rig runtime to own model calls, the agent loop, tool dispatch,
and conversation handling. Keep application identity, transport, persistence
policy, and document ingestion outside the model's control.

## Choose the relevant guide

- For dependencies, a first agent, prompting, or run results, read
  [references/runtime.md](references/runtime.md) and adapt
  [assets/conversation.rs](assets/conversation.rs).
- For retained conversations or tool authoring, read
  [references/memory-and-tools.md](references/memory-and-tools.md).
- For vector retrieval, query selection, or RAG boundaries, read
  [references/retrieval.md](references/retrieval.md).

Locate the target project's Rig source revision before changing its imports.
These instructions describe this checkout, not a guaranteed registry release.
Keep companion crates on the same source revision. Prefer the target project's
existing model provider, tools, and memory backend over replacing them with
the demonstration defaults.

`agent.prompt(...)` returns an `AgentRunner`. Use per-run settings for
conversation identity, turn limits, hooks, and model overrides. A model-call
budget counts tool continuations too; tool execution and a final answer may
require multiple calls.

Configure memory explicitly when history must persist between prompts.
Conversation memory and vector indexes are separate. Registering
`dynamic_context` or a search tool does not index or save conversations.

Use `PortableTool` for tools needing only owned arguments. Use the contextual
`Tool` contract when a call needs trusted host context. Tool descriptions guide
model decisions; enforce authorization inside the host/tool implementation.
Do not treat model-generated user IDs, filters, or tool calls as authorization.

For embedded Qdrant, implement retrieval with the existing `VectorStoreIndex`
and `InsertDocuments` contracts. Avoid introducing a parallel `KnowledgeStore`
trait or MCP server just to connect an in-process backend.

Validate a focused offline behavior using the existing mock models and example
tests. Run only the target project's relevant checks. Do not add live provider
or messaging sends to an ordinary test run.

## Portability

Copy this entire directory into the target environment's skill directory,
such as `~/.codex/skills/rig-agent`, and invoke `$rig-agent`. Retain the bundled
references and assets. Source paths in the guides are relative to the chosen
Rig checkout. The skill is usable independently of the other Rig skills.
