# rig-messaging implementation plan

Specification for the implementing agent. The working directory is the rig repo
(`/home/swind/Program/rig`). OpenAB source is **read-only reference** at
`/home/swind/Program/openab`.

Read rig's `AGENTS.md`, `DEVELOPING.md`, and `tests/README.md` first. Where this
document conflicts with them, rig's rules win; state the conflict in your final
report. Do not commit, stage, or push unless the user explicitly asks.

## 1. Goal

Let a rig `Agent` receive user input from several chat platforms and send replies
and status back to the originating platform.

```
platform event -> platform-specific normalization -> Inbound -> Gate -> per-conversation lock -> rig Agent (stream) -> Egress -> ChatAdapter -> platform API
                                                                                                          \-> ReactionHook (on_dispatch etc.) -> reaction status
```

Ingress and egress are separate concerns:

- **Ingress** is platform code: a serenity `EventHandler`, a Slack Socket Mode callback, a webhook handler. It converts a platform event into an `Inbound` and calls `ChatRouter::handle`. The core crate owns no receive loop and `ChatAdapter` has no `recv()`.
- **Egress** is the core: `ChatRouter` and `egress` call `ChatAdapter` for outbound operations only (send, edit, delete, reactions).

This project is messaging-platform transports and conversation routing for rig agents. It is not a port of OpenAB.

The crate is named `rig-messaging`, not `rig-chat`, because rig already uses "chat" for the LLM conversation interface (for example `pub trait Chat` in `rig-agent/src/integrations/cli_chatbot.rs`).

In scope:

- Multi-platform input (Discord first, then Slack).
- Conversation history via rig's `ConversationMemory`, keyed by the session key as `conversation_id`.
- Streamed output: placeholder message plus timed edits, message splitting, table conversion.
- Reaction status, driven by an `AgentHook`.

## 2. Out of scope (do not add)

| Not built | Add when |
|---|---|
| Permission requests, human approval | Tools need restricting; use `DispatchAction::Deny` in `on_dispatch` |
| ACP, external agent CLIs, child process management | Never |
| Session pool, TTL, eviction | Never; rig's memory is the session |
| Message batching (OpenAB `Thread`/`Lane` modes) | Users actually send bursts that cause redundant turns |
| Multi-bot detection, bot turn limits | Bot-to-bot conversation is required |
| `[[reply_to:..]]` and other output directives | Needed; port OpenAB `directives.rs` then |
| cron, hooks, MCP facade, control plane, secrets | Never |
| Slack native streaming, assistant status | First version uses post-then-edit |
| Append-only (token by token) streaming for terminals | A terminal chat product is needed; use rig's `ChatBotBuilder` (`rig-agent/src/integrations/cli_chatbot.rs`), which already streams |
| `error_display.rs`, `redact.rs` | First version reports errors as one line: `⚠️ {error}` |

## 3. Verified facts

Re-verify before relying on them. If the code disagrees, the code wins.

### rig side (`/home/swind/Program/rig/crates`)

- `Agent` is not generic (`examples/discord_bot/src/discord_bot.rs` holds `agent: Agent`).
- Call shape: `agent.prompt(msg).conversation(id).add_hook(h).stream()`.
  - `AgentRunner::conversation`: `rig-agent/src/agent/runner.rs:331`.
  - `AgentRunner::add_hook`: `runner.rs:122`. It is **per-run** and stacks on top of the agent's own hooks. This is the correct way to bind one reaction controller per message to one run.
  - `AgentRunner::stream()`: `agent/streaming.rs:276`, returns `StreamingResult`.
  - `.history(...)` bypasses conversation memory (near `runner.rs:172`).
- Stream item type `MultiTurnStreamItem` (`agent/streaming.rs:43`):
  - `StreamAssistantItem(Item<StreamEvent>)`, where `StreamEvent` is `Start | Text | Reasoning | Arguments | End` (`rig-core/src/streaming/event.rs:70`).
  - `ToolCall`, `ToolExecutionCommitted` (**not real time**; appears only after the tool batch settles), `StreamUserItem`, `CompletionCall`.
  - `ModelTurnRetried{turn}`: text already emitted for that turn is provisional; the consumer must discard it.
  - `FinalResponse(PromptResponse)`: terminal; `.output()` is the final text.
  - Errors arrive as a stream `Err(StreamingError)`, also terminal.
- `AgentHook` (`agent/hook.rs:957`) methods available: `on_run_start`, `on_run_settled`, `on_text_delta`, `on_reasoning_delta`, `on_tool_call_delta`, `on_dispatch` (`:1092`), `on_outcome` (`:1102`).
  - `DispatchEvent.kind` is `&EffectKind`: `EffectKind::ToolCall{name,args}` or `EffectKind::Completion{..}` (`rig-core/src/effect/mod.rs:697`).
  - `on_dispatch` is **real time**. The default `observes()` returns true for tool and completion dispatch; **do not override it to false** (`hook.rs:1114`).
  - `HookContext` exposes `run_id`, `is_streaming`, `scratchpad`.
- `ConversationMemory` (`rig-core/src/memory.rs:86`): `load`, `append`, `clear`. Append runs synchronously before the reply and is not transactional. There is **no per-conversation serialization** (a search of `engine.rs` and `memory.rs` found only the internal lock of the in-memory map).
- `UserContent` has `Text`, `Image`, `Audio`, `Video`, `Document` (`rig-core/src/completion/message.rs:127`).
- Test utilities: `rig-agent/src/test_utils` (re-exports `rig_core::test_utils`, including mock models and mock tools).
- rig already ships a terminal chat loop: `ChatBotBuilder` in `rig-agent/src/integrations/cli_chatbot.rs`. It streams tokens and keeps history in a caller-owned `&mut Vec<Message>`. The stdio adapter in Phase 2b is a test harness for the messaging pipeline, not a replacement for it.
- **serenity must stay out of the workspace.** The root `Cargo.toml` `exclude` list says `examples/discord_bot` is its own workspace because serenity 0.12.5 pins a `rustls` major with unpatched advisories. The Discord adapter must follow the same pattern.

### OpenAB side (read-only reference)

- `ChatAdapter` trait: `crates/openab-core/src/adapter.rs:335`.
- Reaction controller: `crates/openab-core/src/reactions.rs` (276 lines).
- OpenAB reaction transitions (`adapter.rs:767`, `:995`, `:1008`, `:1052`, `:656`):

  | Moment | Call |
  |---|---|
  | Message arrives | `set_queued` |
  | Turn start, Thinking, ToolDone | `set_thinking` (debounced) |
  | ToolStart | `set_tool(name)` (debounced) |
  | Delivery succeeded | `set_done` |
  | Delivery failed | `set_error` |
  | Afterwards | wait `done_hold_ms` or `error_hold_ms`, then `clear` if configured |

- Pure functions with no `crate::` dependencies, safe to copy: `format.rs` (471 lines, `split_message`) and `markdown.rs` (349 lines, `convert_tables`, `TableMode`).
- Defaults: emoji queued 👀, thinking 🤔, tool 🔥, coding 👨‍💻, web ⚡, done 🆗, error 😱; stall emoji 🥱 (10 s) and 😨 (30 s); `debounce_ms=700`, `stall_soft_ms=10000`, `stall_hard_ms=30000`, `done_hold_ms=1500`, `error_hold_ms=2500`.
- Both projects are MIT. Keep the original copyright notice (`Copyright (c) 2026 openabdev`) in copied files and add an attribution note to the crate README.

## 4. Layout

```
crates/rig-messaging/                 # new companion crate, platform-neutral, no serenity
  src/lib.rs
  src/types.rs
  src/adapter.rs
  src/gate.rs
  src/router.rs
  src/egress.rs
  src/reactions.rs               # controller + ReactionHook
  src/format.rs, src/markdown.rs # copied from OpenAB
  src/<module>/tests.rs          # sibling test files, per rig rules
examples/messaging_stdio/             # workspace member, terminal reference adapter (Phase 2b)
examples/messaging_discord/           # its own workspace (mirror examples/discord_bot), serenity adapter
```

- The crate name `rig-messaging` is decided (see section 1).
- The core crate's `Cargo.toml` must not depend on serenity, Slack SDKs, or any platform library. Check with `cargo tree -p rig-messaging`.
- Per rig `AGENTS.md` ("Repository Shape"), adding a companion crate means updating the root `Cargo.toml` dependency, feature, facade re-export, `default-members` (if applicable), README, crate docs, and examples. Check each; do not skip any.
- Decide the Slack adapter location in Phase 5 (inside the workspace using `rig-tungstenite` and `rig-reqwest`, or its own workspace).
- This crate targets native platforms only. Look at how an existing native-only crate (for example `rig-bedrock`) declares that and do the same. Say so in the crate docs.

### rig rules that apply

- Workspace clippy forbids `unwrap`, `expect`, `todo`, `unimplemented`.
- New fallible APIs must not use `String` as the error type; use a `thiserror` enum.
- No TODOs, stubs, or speculative APIs.
- Tests live in sibling files: `#[cfg(test)] mod tests;` with the body in `foo/tests.rs`. CI enforces this with `cargo xtask check-test-layout`.
- Doc style: short sentences, no em-dashes, no history or comparisons.
- The OpenAB code to copy contains an `unwrap()` (one in `format.rs`) and inline `mod tests {}` blocks. Both must be rewritten to comply.

## 5. Module specifications

### 5.1 `types.rs`

```rust
pub struct ChannelRef {
    pub platform: String,
    pub scope_id: Option<String>,   // Slack workspace, Discord guild, tenant; None for DMs or single-scope platforms
    pub channel_id: String,
    pub thread_id: Option<String>,  // Slack thread_ts; None on Discord (a thread is its own channel)
}
pub struct MessageRef { pub channel: ChannelRef, pub message_id: String }
pub struct Sender { pub id: String, pub name: String, pub is_bot: bool }
pub struct Inbound {
    pub message: MessageRef,        // original triggering message; reactions attach here
    pub reply_channel: ChannelRef,  // outbound destination and conversation identity
    pub sender: Sender,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub is_dm: bool,
    pub is_thread: bool,            // whether the original message arrived in a thread
    pub mentions_bot: bool,
}
```

`ChannelRef::session_key()` uses a versioned, unambiguous encoding. Start with
`v1:` and encode exactly four fields in order: platform, scope_id, channel_id,
thread_id. A present value is `{UTF-8 byte length}:{value}`; an absent optional
value is `-:`. Concatenate the encoded fields without additional separators.
For example, platform `stdio`, no scope, channel `local`, no thread becomes
`v1:5:stdio-:5:local-:`. `None` and `Some("")` remain distinct.

Use `Inbound.reply_channel.session_key()` for memory and locking. Keep
`Inbound.message` unchanged when the reply destination is a newly created thread.

- The key **always contains `channel_id`**. A Slack `thread_ts` is unique only within its channel, so `{thread_ts}` alone can collide across channels. (OpenAB's `platform:{thread_id or channel_id}` has this weakness; do not copy it.)
- It does not include the sender, so everyone in a thread shares one conversation.
- The key is persisted as the `ConversationMemory` id. Changing its format later is a data migration, so get it right now.

```rust
pub struct Attachment {
    pub filename: String,
    pub mime: String,
    pub size: Option<u64>,          // lets callers enforce limits before downloading
    pub source: AttachmentSource,
}
pub enum AttachmentSource { Bytes(bytes::Bytes), Url(String) }   // no File variant until a caller needs it
```

`bytes` is already a rig-core dependency. A core type must not force every adapter to download eagerly; the first Discord adapter may still download small attachments. Convert to rig `UserContent` (image, document, and so on). Support depends on the provider; replace unsupported types with a one-line text note.

### 5.2 `adapter.rs`: `ChatAdapter`

```rust
pub trait ChatAdapter: WasmCompatSend + WasmCompatSync + 'static {
    fn platform(&self) -> &'static str;
    fn message_limit(&self) -> usize; // positive limit, measured in Unicode scalar values
    fn send<'a>(&'a self, ch: &'a ChannelRef, text: &'a str) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>>;
    fn edit<'a>(&'a self, m: &'a MessageRef, text: &'a str) -> WasmBoxedFuture<'a, Result<(), ChatError>>; // Unsupported if unavailable
    fn delete<'a>(&'a self, m: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>>;
    fn add_reaction<'a>(&'a self, m: &'a MessageRef, emoji: &'a str) -> WasmBoxedFuture<'a, Result<(), ChatError>>;
    fn remove_reaction<'a>(&'a self, m: &'a MessageRef, emoji: &'a str) -> WasmBoxedFuture<'a, Result<(), ChatError>>;
    // Capabilities are reported by methods, not by a hard-coded list of platform names.
    fn supports_edit(&self) -> bool { true }
    fn supports_reactions(&self) -> bool { true }
    fn renders_native_tables(&self) -> bool { false }
}
```

- No `create_thread` on the trait unless a platform flow forces it. On Discord, opening a thread when the bot is mentioned in a channel belongs to the adapter's event-to-`Inbound` step: apply Gate first, create the thread, then point `Inbound.reply_channel` at it. Preserve the original `Inbound.message`.
- Use `rig_core::wasm_compat::{WasmCompatSend, WasmCompatSync, WasmBoxedFuture}`. Boxed futures keep the trait usable as `dyn ChatAdapter` and follow existing Rig conventions without adding `async_trait`. Native-only support does not waive the repository rule for WASM-compatible bounds.

### 5.3 `gate.rs`

```rust
pub struct Gate { pub allowed_channels: Option<HashSet<String>>, pub allowed_users: Option<HashSet<String>>, pub allow_bots: bool }
impl Gate { pub fn allows(&self, m: &Inbound, bot_user_id: &str) -> bool }
```

Rules, in order: not the bot's own message; if `is_bot`, require `allow_bots`; channel allowlist; user allowlist (bots are exempt); outside a DM and outside an existing thread, require `mentions_bot`. Use `m.is_thread` to recognize an existing thread. `None` or an empty allowlist
imposes no restriction. Channel allowlists match the original
`m.message.channel.channel_id`, before any thread creation; follow-up messages
must allow their thread channel explicitly. Parent-channel inheritance is outside
this version.

The platform ingress must apply this same Gate before creating threads or
downloading attachments. The router rechecks it against the unchanged original
message metadata. A newly created reply thread does not change `m.is_thread`.

### 5.4 `router.rs`

```rust
pub struct ChatRouter { agent: Agent, gate: Gate, locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>, cfg: ChatConfig }
impl ChatRouter {
    pub fn allows(&self, m: &Inbound, bot_user_id: &str) -> bool;
    pub async fn handle(&self, adapter: Arc<dyn ChatAdapter>, m: Inbound, bot_user_id: &str) -> Result<(), ChatError>;
}
```

Construction requires an `Agent` configured with `.memory(...)` or
`.memory_handler(...)`. `.conversation(key)` only chooses an id; it does not
install a memory backend. State this caller invariant in the router docs and
configure memory in every example and history test.

`ChatConfig` contains the egress `table_mode`, `attachment_mime_types`, and,
from Phase 3, `ReactionConfig`. The MIME allowlist defaults to empty; callers
explicitly enable formats their model supports for both bytes and URLs. Unknown
or disabled MIME types become a short attachment-unavailable text note. Rig's
Agent exposes no universal provider input-media capability contract to query.
Keep the edit interval and failure threshold fixed as specified in section 5.5.
Gate is supplied separately. The router preserves the Agent's model-call budget;
tool examples and tests configure `.default_max_turns(3)` explicitly.

`handle` flow:

1. `self.allows(&m, bot_user_id)` delegates to `gate.allows`; on failure return silently. Validate the adapter message limit before constructing a stream; return a typed error for invalid configuration.
2. Create `StatusReactions` (section 6) and call `set_queued()`. This happens **before** acquiring the lock, so queued messages show 👀 while they wait.
3. Acquire the lock for this `session_key` and hold it until the turn ends. **Do not skip this.** rig loads history before the turn and appends after it, so concurrent turns on one `conversation_id` read stale history and interleave writes.
   - Lookup and cloning the per-key `Arc` happen under the table mutex. Release
     the table mutex before awaiting the per-conversation lock.
   - After the turn, drop the conversation guard. Acquire the table mutex,
     drop the handler's local `Arc`, and remove the entry only when the table's
     `Arc::strong_count == 1`. Check and removal happen in the same critical
     section. Waiting handlers retain their own `Arc`, so their lock cannot be
     removed. Perform cleanup on both success and error paths.
   - Serialization follows lock acquisition order. Separately spawned handlers
     do not guarantee platform arrival order. Tests establish which turn has
     acquired the lock before starting the next one.
4. `set_thinking()`.
5. Build the prompt. Keep the model context free of routing data.
   - The message starts with a minimal speaker marker, for example `[alice (U123)]`, followed by the user text, then attachments. Include the stable id because two people can share a display name.
   - Do **not** put `platform`, `channel_id`, `thread_id`, `message_id`, or workspace/guild ids into the conversation. They cost tokens on every turn, stay in history forever, and the model does not need them.
   - Routing data stays in application state (`Inbound`, the router's per-run variables). If a tool later needs it, pass it through `AgentRunner::tool_context` (`runner.rs:166`), not through history.
6. `agent.prompt(msg).conversation(key).add_hook(ReactionHook::new(reactions.clone())).stream()`.
7. `egress(...)` using `m.reply_channel` (section 5.5).
8. From the egress result call `set_done()` or `set_error()`.
9. Release the lock and clean up its table entry. Then, if `remove_after_reply`
   is true, wait `done_hold_ms` or `error_hold_ms` and `clear()`. The reaction
   hold must not delay the next conversation turn.

Run `handle` with `tokio::spawn` from the adapter's event callback so the platform event loop never blocks. Log any returned error in that task.

### 5.5 `egress.rs`

`async fn egress(adapter, channel, stream: StreamingResult, cfg) -> Result<(), ChatError>`

Behavior:

- `streaming = adapter.supports_edit()`.
- The router validates a positive `message_limit` before constructing the stream; reject zero with a typed configuration error. Egress may assume this invariant and must still drain any stream it receives.
- Accumulate `buf`. On `StreamAssistantItem(Item::Event(StreamEvent::Text{text,..}))`, `buf.push_str`.
- Streaming mode:
  - Send a placeholder message ("…") on the first text.
  - Edit at most once every 1500 ms. Reserve up to 100 characters using
    `limit.saturating_sub(100).max(1)`. If content exceeds that preview size,
    show only the tail, slicing at Unicode scalar boundaries.
  - If placeholder creation fails, record it for debugging, stop preview
    operations, and continue consuming the stream for final delivery.
  - After 3 consecutive edit failures, stop editing and deliver at the end instead.
- `ModelTurnRetried` -> `buf.clear()` and, if a usable placeholder exists,
  edit it to "…" immediately. This reset is exempt from the preview throttle;
  record reset failures and continue consuming the stream.
- `FinalResponse(r)`: use `r.output()` as the final text (it is authoritative).
  Inspect `r.memory_append()`. For `MemoryAppend::Failed`, deliver the answer
  followed by a short warning that history persistence was not acknowledged,
  then return a typed error so the router sets the error reaction. Do not retry
  the append: the backend may already have written it. With no append outcome,
  report a missing memory acknowledgement rather than claiming history was saved;
  this indicates a violated router caller invariant. Deliver the answer and the
  same warning, then return a typed error. Both warnings use normal chunking.
- Final delivery:
  1. Tables: if `renders_native_tables()` use `TableMode::Off`, otherwise `cfg.table_mode`.
  2. `format::split_message(text, adapter.message_limit())`.
  3. With a placeholder: `edit` the first chunk; on failure attempt `delete`,
     then `send` even if deletion failed. Send the remaining chunks. A failed
     deletion leaves stale provisional text and makes the result an error even
     if the replacement was sent; log its message reference.
  4. Without a placeholder: `send` every chunk.
  5. Empty final text: send a short explanation. Never stay silent.
- Stream `Err(e)`: deliver `⚠️ {e}` through the same chunking and placeholder
  replacement path, then return a typed stream error even when that delivery
  succeeds. Log any delivery failure as well. A failed agent run is not promised
  to have been appended to memory.
- EOF without `FinalResponse` or `Err` is a typed unexpected-end error. Replace
  any placeholder with a short error explanation through the same delivery path.
- Any final chunk that remains undelivered must make `egress` return `Err`.
  Preview send/edit/reset failures are recoverable if final delivery succeeds.
  Do not classify a successfully recovered preview failure as failed delivery.
- Ignore every other `MultiTurnStreamItem` (`_ => {}`).
- Tool status is not egress's job; the hook in section 6 handles it.

Internal structure (functions in `egress.rs`, no extra structs or public API):

- `render_chunks(text, limit, table_mode) -> Vec<String>`: pure function. Table conversion plus `split_message`. Testable with no adapter.
- A streaming accumulator: collects text deltas, handles `ModelTurnRetried`, tracks placeholder and edit timing.
- `deliver(adapter, channel, placeholder, chunks)`: send, edit, delete fallback, multi-chunk delivery, error propagation.
- `egress` only wires these together, so new Slack or Discord behavior does not accumulate in one function.

Draining requirement:

- **Always read the stream to its terminal item (`FinalResponse` or `Err`), even after edits or deliveries have failed.** rig appends to `ConversationMemory` before it emits `FinalResponse`. If `egress` returns early and drops the stream, that turn may never reach memory, and the next turn will not see it.
- Preview failures are recorded without returning early. After termination, report stream, memory, or unresolved final-delivery failures; a recovered preview failure does not invalidate the final reply.
- The per-conversation lock (section 5.4) must cover the whole span: history load, agent run, stream consumption, and the final memory append.

### 5.6 `format.rs`, `markdown.rs`

Copy from OpenAB. Allowed changes only:

- Remove `unwrap` and other workspace lint violations from non-test code.
- Move tests to sibling files (`format/tests.rs` and so on), keeping every existing test case.
- Add the copyright notice and attribution.
- Remove configuration deserialization from `TableMode`; no serde config API in this version.
- Keep every original behavior test. Preserve helper behavior while making lint fixes.
- Correct the verified small-limit fence overflow: when a fence wrapper cannot fit,
  split the original text without adding fence wrappers. Test limits 1 through 15.
  This fallback preserves the hard message limit; balanced fences are guaranteed
  only when their wrapper can fit.
- Dependencies: `unicode-segmentation`, `pulldown-cmark`, `unicode-width`. Prefer entries already in the root `[workspace.dependencies]`; add new ones only if absent.

## 6. Reactions

### 6.1 Controller `StatusReactions`

Port OpenAB `reactions.rs`, preserving behavior:

- Methods: `set_queued` (immediate), `set_thinking` (debounced), `set_tool(name)` (debounced), `set_done`, `set_error`, `clear`.
- State: `current` (the applied emoji) and `finished` (later updates are ignored once finished).
- Debounce: do not reapply the same emoji; a new emoji is applied after `debounce_ms`, and a newer update within that window cancels the pending one.
- Apply order: `add_reaction(new)` first, then `remove_reaction(old)`, so there is never a gap.
- Stall timers reset on every transition. After `stall_soft_ms` without progress apply 🥱; after `stall_hard_ms` apply 😨.
- `set_done` adds a random mood emoji in addition to 🆗.
- Swallow and `tracing::debug` every adapter error. A failed reaction must never affect the reply.
- `enabled` is derived from `adapter.supports_reactions()` and config. When disabled every method returns immediately.
- Tool-name classification (case-insensitive substring):
  - coding emoji: `exec`, `process`, `read`, `write`, `edit`, `bash`, `shell`.
  - web emoji: `web_search`, `web_fetch`, `web-search`, `web-fetch`, `browser`.
  - otherwise the generic tool emoji. Web is checked before coding.

Config (replaces OpenAB's `ReactionEmojis` and `ReactionTiming`):

```rust
pub struct ReactionConfig { pub enabled: bool, pub emojis: ReactionEmojis, pub timing: ReactionTiming, pub remove_after_reply: bool }
```

Provide every default through `Default` using the values in section 3. No serde in the first version; add it when users need to load config from a file.

`rand`: use an RNG source rig already depends on (for example `fastrand`, already a rig-core dependency). Do not add a new dependency.

### 6.2 `ReactionHook` (maps rig events to the controller)

```rust
pub struct ReactionHook { ctl: Arc<StatusReactions> }
impl AgentHook for ReactionHook { ... }
```

| rig hook | Action | OpenAB equivalent |
|---|---|---|
| `on_dispatch`, `EffectKind::ToolCall{name,..}` | `ctl.set_tool(name)`, return `DispatchAction::Proceed` | `ToolStart` |
| `on_dispatch`, `EffectKind::Completion{..}` | `ctl.set_thinking()`, return `Proceed` | start of each model call |
| `on_outcome`, tool call result | `ctl.set_thinking()`, return `OutcomeAction::Proceed` | `ToolDone` |
| `on_reasoning_delta` | `ctl.set_thinking()`, return `ObservationAction::Continue` | `Thinking` |

Requirements:

- **A hook must not wait on the network.** `set_tool` and `set_thinking` are debounced and send through `tokio::spawn`; the hook only updates internal state and returns. `on_dispatch` sits on the run's critical path, and blocking it slows every turn.
- Always return `Proceed` or `Continue`. The hook observes and never changes the run.
- Do not override `observes()`. The default already covers `ToolDispatch` and `CompletionDispatch`.
- Do not drive reactions from `MultiTurnStreamItem::ToolExecutionCommitted`; it is emitted after the batch settles, not in real time.
- `set_done` and `set_error` are **not** called from the hook. The router calls them after egress (section 5.4 step 8). When delivery fails, the run itself succeeded but the user must still see an error.
- Adopted deviation (mark it as different from OpenAB): a long text-only answer has no tool or reasoning transition, so the stall timer can wrongly show 🥱. In `on_text_delta`, call `ctl.touch()` to reset the stall timer, rate limited to once per second with an atomic timestamp so every token does not take the lock.
- Each run has its own `ReactionHook` and `StatusReactions`; nothing is shared between runs. Attach the hook per run with `add_hook`; do not register it on `AgentBuilder`.

## 7. Discord adapter (`examples/messaging_discord`)

- Its own workspace with an empty `[workspace]` table, mirroring the comments and lints in `examples/discord_bot/Cargo.toml`.
- References: `examples/discord_bot/src/discord_bot.rs` (serenity `EventHandler`) and OpenAB `crates/openab-core/src/discord.rs`.
- Implement `ChatAdapter`:
  - `message_limit` = 2000, counted in Unicode characters.
  - `send`, `edit`, `delete`, `add_reaction`, `remove_reaction` map to serenity HTTP calls.
  - `renders_native_tables` = false; egress converts tables.
- Event to `Inbound`:
  - Ignore the bot's own messages.
  - Thread detection: a channel is a thread **only if `thread_metadata` is present; never infer it from `parent_id`** (OpenAB `AGENTS.md` rule).
  - Normalize original metadata first, set `is_thread` from `thread_metadata`,
    and initialize `reply_channel` to the original channel. Call
    `router.allows(&inbound, bot_user_id)` before any thread creation or attachment
    download. Outside DMs and existing threads, create a thread for an allowed
    mention and set only `reply_channel` to that thread. Later messages in that
    thread need no mention. Reactions remain attached to the original message.
  - Download attachments to bytes. Resizing images is optional in the first version; enforce a size limit at minimum (OpenAB `media.rs` has a full implementation).
- Discord limits: 2000 characters per message, 25 options per select menu (this plan uses no menus).

## 8. Phases and acceptance

Each phase is independently verifiable. Finish one before starting the next.
Commit each phase after its focused checks pass. Stage only files belonging to
this implementation. Do not push or open a PR. Live acceptance requires platform
credentials; record missing live evidence explicitly, and continue offline work.

| Phase | Tasks | Commit boundary |
| --- | --- | --- |
| 0 | Define routing types, attachment sources, object-safe adapter and typed errors; wire native facade feature and docs; test session identity | Foundation and focused compile/lint checks |
| 1 | Port licensed splitting/table helpers; move all original tests; resolve workspace lint violations | Output helpers and original regression tests |
| 2 | Implement Gate, prompt conversion, per-session locks and draining egress; fake-adapter/mock-agent tests for ordering, failures and memory | Routing and egress with regressions |
| 2b | Add stdio adapter, ordered stdin handling and memory; verify degraded capabilities offline | Runnable terminal harness |
| 3 | Implement reaction controller and per-run hook; debounce/stall/concurrency tests | Status reactions and hook integration |
| 4 | Add isolated Discord package, ingress checks, threads, attachments and HTTP egress; offline tests and separate build | Discord integration |
| 5 | Inspect Slack reference; choose adapter location; implement Socket Mode normalization and adapter; offline tests and build | Slack integration |

The phase tests are the acceptance criteria below. Integration examples document
credentials and live commands. Final verification includes package tests, lint,
format, test layout, facade compilation and platform dependency inspection.

### Phase 0: skeleton

- Create `crates/rig-messaging` and wire it into the workspace and facade (section 4).
- `types.rs` and `adapter.rs` (including `ChatError`).
- Accept when `cargo check -p rig-messaging` and `cargo clippy -p rig-messaging` pass.

### Phase 1: output pieces

- Copy `format.rs` and `markdown.rs` and bring them in line with rig rules.
- Accept when all original test cases pass and `cargo xtask check-test-layout` passes.

### Phase 2: routing and egress (no reactions)

- `gate.rs`, `router.rs` (with the per-key lock), `egress.rs`.
- Test with a fake `ChatAdapter` that records calls and a mock model from `rig_core::test_utils`.
- Accept when tests show:
  - Two concurrent messages in one session: the second turn's history contains the complete first turn.
  - Streaming: the placeholder is sent once, edits respect the throttle, and the final text replaces the placeholder.
  - `ModelTurnRetried` clears the buffer.
  - Over-limit text is split and code fences are balanced in every chunk.
  - Empty final text produces an explanation.
  - A stream `Err` sends `⚠️` and returns `Err`.
  - The lock table holds no entry after the last handler, on success and error;
    a waiting handler prevents removal, and a new arrival during cleanup cannot
    obtain a different lock for the same active conversation.
  - Gate recognizes Discord threads with `thread_id = None` and `is_thread = true`.
  - Session keys distinguish absent fields, empty fields, colons, and Unicode.
  - Failed previews followed by successful final delivery return success.
  - Failed placeholder deletion still attempts replacement delivery and returns
    an error. Stream errors replace placeholders and respect the message limit.
  - Unexpected EOF produces an explanation and returns an error.
  - Memory append failure delivers the answer and persistence warning, returns
    an error, and does not retry the append.
  - Egress drains the stream to `FinalResponse` even when every `edit` and `send` fails: after such a turn, the next turn's history still contains the failed turn in full.
  - `render_chunks` has direct unit tests (tables, splitting, code fences) with no adapter involved.
  - The core crate compiles with no platform dependency (`cargo tree -p rig-messaging` shows no serenity or Slack SDK).

### Phase 2b: stdio reference adapter

Purpose: run the whole pipeline (Inbound, Gate, lock, rig, egress) in a terminal with no bot token, and prove the degraded path for a platform that cannot edit or react. It is a development harness, not a supported product.

Create `examples/messaging_stdio` as a workspace member (the root `members` already includes `examples/*`). It depends on `rig-messaging`, `rig-agent`, `rig-core` for conversation memory, a provider crate already used by other examples, and `tokio`. Take the model from the environment the way other rig examples do (for example `OpenAI::from_env()`).

`ChatAdapter` implementation:

- `platform()` returns `"stdio"`.
- `message_limit()` returns `usize::MAX`.
- `send` prints the text to stdout and returns a `MessageRef` with an incrementing counter as `message_id`.
- `edit` and `delete` return `ChatError::Unsupported`.
- `add_reaction` and `remove_reaction` are never called.
- `supports_edit()` is false and `supports_reactions()` is false.
- `renders_native_tables()` is false, so tables become code blocks.

Input:

- Configure an in-memory `ConversationMemory` on the Agent.
- Each stdin line becomes one `Inbound`: `platform = "stdio"`, `scope_id = None`, `channel_id = "local"`, `thread_id = None`, sender id `"local"` and name from `$USER` (fall back to `"user"`), `is_bot = false`, `is_dm = true`, `is_thread = false`, `mentions_bot = false`. Both message and reply channels are `stdio/local`; pass a distinct bot user id such as `"rig-bot"` to `handle`.
- Await `ChatRouter::handle` for each line in input order. This keeps piped input
  deterministic. Concurrent lock behavior is verified by Phase 2's controlled
  tests.
- EOF exits after all accepted lines have completed.

Not built here, and do not add: token-by-token display, a prompt line editor, slash commands. For a streaming terminal chat, use `ChatBotBuilder`. Say so in the example's module docs.

Accept when:

- `printf 'first\nsecond\n' | cargo run -p messaging_stdio` prints both replies in order, and the second reply's history contains the first turn.
- Egress takes the send-once path: exactly one `send` per reply chunk, no placeholder, no `edit` call.
- No reaction call is made (verify with a counting wrapper in a test, or by the fact that `add_reaction` is unreachable).
- The example builds and runs with no serenity or Slack dependency in `cargo tree -p messaging_stdio`.

Phase 3 must re-run this example: with `supports_reactions() == false` the reaction controller must be disabled and nothing else may change.

### Phase 3: reactions

- `StatusReactions` and `ReactionHook`, wired into the router (section 5.4 steps 2, 4, 8).
- Test with a fake adapter recording add/remove order. Accept when:
  - Plain text reply: 👀 -> 🤔 -> 🆗 (plus mood emoji), always add before remove.
  - With a tool call: 👀 -> 🤔 -> 🔥 or 👨‍💻 or ⚡ (by tool name) -> 🤔 -> 🆗. Trigger it with a mock tool.
  - Tool names classify per section 6.1.
  - Several switches inside one debounce window apply only the last one.
  - When egress returns `Err`, the result is 😱, not 🆗.
  - No progress beyond `stall_soft_ms` shows 🥱 (use `tokio::time::pause`).
  - With `supports_reactions() == false`, `add_reaction` is never called.
  - Two concurrent runs do not affect each other's reactions.
  - With `add_reaction` blocked behind a test barrier, dispatch and delta hook
    callbacks still complete. Release the barrier for router finalization;
    final reaction delivery is outside the hook timing assertion.
- Use paused time and controlled dispatches for exact reaction sequences; fast turns may debounce intermediate thinking or tool reactions away.

### Phase 4: Discord adapter

- `examples/messaging_discord` as described in section 7.
- Configure conversation memory and `.default_max_turns(3)` on the example Agent.
- Accept when `cargo build --manifest-path examples/messaging_discord/Cargo.toml`
  passes and a live test covers mention trigger, follow-up inside a thread,
  long-reply splitting, table conversion, and reaction status. Include a tool
  call and record the reaction sequence on the original triggering message.
  Confirm rejected input creates no thread and downloads no attachments.

### Phase 5: Slack adapter (decide separately)

- Use Socket Mode. Slack differs from Discord in message limit and tables (OpenAB `slack.rs` sends Block Kit `markdown` blocks with a limit near 11,900 characters and `renders_native_tables` = true).
- Acceptance: adding Slack **requires no change** to `router.rs` or `egress.rs`. If it does, the `ChatAdapter` abstraction leaked; fix the interface first.

## 9. Verification commands

Per rig `AGENTS.md`:

```bash
cargo nextest run --locked --profile local -p rig-messaging
cargo clippy -p rig-messaging --all-targets
cargo fmt --check
cargo xtask check-test-layout
```

`examples/messaging_stdio` is a workspace member:

```bash
cargo build -p messaging_stdio
```

`examples/messaging_discord` is its own workspace; build it separately:

```bash
cargo build --manifest-path examples/messaging_discord/Cargo.toml
```

Do not run `--all-features` across the whole workspace; that belongs to CI. For documentation-only changes, do a diff and link review only.

## 10. Decisions for the user

Decided by the user: crate name `rig-messaging`, location `crates/rig-messaging`.

Implementation defaults:

- Expose the companion through the facade as required by `AGENTS.md`, using an
  optional `messaging` feature. Its dependencies must enable the Agent support
  required by the re-export.
- Use Rig's WASM-compatible bounds and boxed futures (section 5.2).
- `remove_after_reply` defaults to false, preserving OpenAB's behavior.
- Adopt the `on_text_delta` stall reset from section 6.2 and document the deviation.

The Slack adapter location remains open for Phase 5: inside the workspace or
its own workspace.

## 11. Open risks (report these; do not expand scope on your own)

- `StreamingError` categories were not read; first version presents errors through `Display`.
- Cancelling an in-flight stream: expected to be dropping the stream or aborting the outer task, but it is **unverified** whether rig leaves half-written history when dropped. The first version provides no cancel.
- The default `max_turns` is 1 (`agent/completion.rs`). It counts all model
  calls, including retries and tool continuations. Exhaustion produces
  `PromptError::MaxTurnsError` (`run/mod.rs`), adapted to a stream error. Tool
  examples use `.default_max_turns(3)` and handle exhaustion as a failed run.
- Task abortion may skip normal lock-table cleanup and reaction cleanup.
  The first version exposes no cancellation API; do not claim cancellation
  guarantees or that aborted handlers always leave the lock table empty.
- Which attachment types convert to `UserContent` depends on the provider. Document the fallback for unsupported types in the crate docs.
- Before copying OpenAB code, confirm `/home/swind/Program/openab/LICENSE` (MIT, Copyright 2026 openabdev) and keep the notice.
