# Add messaging to another Rust project

## Select the source

Read the target project's manifest first. This checkout declares Rust 1.98.1
and Rig 0.42.0. Use a source containing the new `Inbound.context` field; do not
assume crates.io or the upstream repository already contains this branch.

The concrete layout below assumes sibling directories `my-bot/` and `rig/`,
where `rig/` is the selected checkout. Adjust paths to the actual layout.

```toml
[package]
name = "my-bot"
version = "0.1.0"
edition = "2024"
rust-version = "1.98.1"

[dependencies]
rig-messaging = { path = "../rig/crates/rig-messaging" }
rig-agent = { path = "../rig/crates/rig-agent", default-features = false }
rig-core = { path = "../rig/crates/rig-core", default-features = false, features = ["reqwest", "rustls"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "io-std", "io-util"] }
```

For a Git dependency, use the user's chosen repository URL and a fixed revision
containing these APIs for every Rig dependency. Use registry versions only
after verifying that the desired release contains the implementation.

The alternative facade dependency is `rig = { path = "../rig", features =
["messaging"] }`. It exports `rig::AgentBuilder`, `rig::messaging`,
`rig::memory`, `rig::providers`, and `rig::wasm_compat`. Choose direct companion
imports or facade imports consistently; the bundled asset uses companions.

## Runnable baseline

Copy [../assets/stdio-bot.rs](../assets/stdio-bot.rs) to the application's
`src/main.rs` with the dependencies above. It is adapted from the checked-in
`examples/messaging_stdio/src/main.rs`. It reads lines from stdin, normalizes
them to `Inbound`, retains in-memory history, and writes final replies to stdout.
It implements an actual stdout transport; unavailable edits and reactions are
explicitly disabled.

Run `cargo check` to verify the selected dependency source. Export
`OPENAI_API_KEY` through the environment, optionally set `OPENAI_MODEL`, then
run `cargo run` and enter a question. Running the app calls the model provider.
No Slack or other messaging account is required. The asset does not load `.env`.

For an offline router test, inspect the checkout's
`examples/messaging_stdio/src/main/tests.rs`. It uses
`rig_core::test_utils::MockCompletionModel`, with the `test-utils` feature,
and checks ordering and retained history without API credentials.

## Existing application wiring

Keep the application's selected model, tools, and memory backend. The essential
construction is:

```rust
use rig_agent::AgentBuilder;
use rig_core::memory::InMemoryConversationMemory;
use rig_messaging::{ChatConfig, ChatRouter, Gate};

// `model` is the application's configured Rig completion model.
let agent = AgentBuilder::new(model)
    .memory(InMemoryConversationMemory::new())
    .build();
let router = ChatRouter::new(agent, Gate::default(), ChatConfig::default());
```

This fragment belongs inside application setup; the asset supplies the complete
OpenAI setup. `InMemoryConversationMemory` does not survive restarts. Replace it
with a suitable existing memory backend when persistence is required. Messaging
does not automatically index conversations in a vector store.

Defaults reject the bot's own messages and other bots. Group messages need a
bot mention or a thread; DMs do not. Missing or empty allowlists impose no
channel/user restriction. Configure `Gate` using stable platform IDs.

`ChatConfig::attachment_mime_types` defaults to `None`, accepting all recognized
media types and preserving bytes or URLs. Use `Some(allowlist)` to restrict MIME
types or `Some` with an empty set to disable media content. Ingress owns bounded
downloads; the router converts unrecognized or excluded attachments into text notes.

For the full delivery and persistence contract, read the selected checkout's
`crates/rig-messaging/CONTRACT.md`. Surface `ChatError` to application diagnostics;
do not automatically replay a whole turn after a delivery or memory error.
