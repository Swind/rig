# Runtime setup and execution

## Dependencies

This checkout declares Rig 0.42.0 and Rust 1.98.1. For sibling directories
`my-agent/` and the chosen `rig/` checkout, a native starter manifest is:

```toml
[package]
name = "my-agent"
version = "0.1.0"
edition = "2024"
rust-version = "1.98.1"

[dependencies]
rig-agent = { path = "../rig/crates/rig-agent", default-features = false }
rig-core = { path = "../rig/crates/rig-core", default-features = false, features = ["reqwest", "rustls"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Adjust paths to the actual checkout. For Git dependencies, pin every Rig crate
to the user's selected repository and revision. Verify implementation presence
before using a registry release; a manifest version is not publication evidence.

Alternatively use the root `rig` facade, whose default features include the
agent runtime and native HTTP setup. It exports `rig::AgentBuilder`,
`rig::AgentRunner`, `rig::providers`, `rig::memory`, and `rig::tool`. The asset
uses companion imports to make the runtime and core responsibilities explicit.

## Start with one agent

Copy [../assets/conversation.rs](../assets/conversation.rs) to `src/main.rs`.
It makes two sequential prompts in one in-memory conversation. Export
`OPENAI_API_KEY`, optionally set `OPENAI_MODEL`, and run `cargo run`. These
calls use the provider account; the application does not load `.env` itself.
Replace the demonstration provider/model with the target application's choice.

`AgentBuilder::new(model).preamble(...).build()` constructs an agent.
`agent.prompt(text).await` and `agent.prompt(text).run().await` both return
`PromptResponse`, including `output`, `usage`, `completion_calls`, `messages`,
and `memory_append`. Read `.output` for the final text.
`messages` is the current run's transcript; it excludes pre-existing history.

Set `.max_turns(n)` on the runner to bound model calls. A tool request followed
by an answer needs at least two calls. Hook-triggered model retries also
consume this budget. Keep the chosen limit aligned with the required workflow.

`.stream()` uses the same agent lifecycle and produces a multi-turn stream.
Read it through the final response and propagate errors. Text deltas are
provisional; retry hooks can reject a turn. Inspect
`crates/rig-agent/src/agent/streaming.rs` for the stream item enum and existing
consumer helpers before writing a new renderer.

## Conditional runtime features

For runtime model selection, inspect
`crates/rig-agent/examples/runtime_model_routing.rs`. It registers models with
`AgentBuilder::named_model(...).model_route(...)` and selects them with an
`AgentHook`. `.using_model(label)` sets one run's initial candidate; routing
hooks can still replace it. Builder hooks apply to later runs, while runner
hooks apply only to that run. The example is credential-free.

For raw provider response inspection or bounded truncation retries, read only
`crates/rig-agent/examples/raw_response_hook.rs` or `retry_on_truncation.rs`.
Keep provider-specific policy out of ordinary transport code.

Use `AgentRunner` for ordinary application integrations. Directly stepping
`AgentRun` is a separate host-owned execution mode and does not automatically
run an agent's hooks, retrieval, tools, or memory. Inspect `crates/rig-agent/README.md`
and `tests/fixtures/agent_run_stepper/` only when that execution mode is requested.
