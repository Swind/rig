# rig-messaging

Native messaging platform transports and conversation routing for Rig agents.
Platform ingress normalizes messages into `Inbound`. `ChatAdapter` provides
outbound send, edit, delete and reaction operations without platform SDKs.

Use the companion directly or enable the facade's `messaging` feature and import
`rig::messaging`. This crate supports native targets only.

Message references retain the original channel for reactions. `reply_channel`
selects the destination and conversation identity. Session keys encode all four
routing fields with lengths so platform identifiers cannot collide.

Message splitting and markdown table rendering are adapted from OpenAB
(Copyright (c) 2026 openabdev), under the MIT license in [LICENSE.OpenAB](LICENSE.OpenAB).

## Routing

Construct `ChatRouter` with an Agent configured with `.memory(...)` or
`.memory_handler(...)`. Use one router for all ingress sharing the same Agent
and history backend. A lock serializes each conversation through final delivery;
independent conversations can run concurrently. Spawned handlers run in lock
acquisition order. The router reclaims locks after normal success and failure.
Task abortion can bypass cleanup; there is no cancellation API.

Gate checks original message metadata. Apply `router.allows` before creating
threads or downloading attachments, then call `handle` with the bot's platform
user id. Channel allowlists match exact original channel ids, including threads.
Rejected inputs produce no model or outbound calls.

Set `ChatConfig::attachment_mime_types` to formats the model accepts for both
bytes and URLs. The default set is empty. Supported image, document, audio and
video MIME types become Rig user content. Other attachments become a short text
note. Ingress owns downloads and size limits.

Egress consumes the stream through its terminal item even when preview operations
fail. It uses the final response as authoritative and sends every reply chunk.
Failed history persistence delivers the answer with a warning and returns an
error without retrying the append. Preview failures recovered by final delivery
return success. Undelivered chunks and undeleted stale placeholders return errors.
Fences are reopened across chunks when their overhead fits; smaller limits split
the original text without adding wrappers.

Run the [stdio harness](../../examples/messaging_stdio) to exercise routing without
a platform bot token. Its mock-agent test runs without provider credentials.
The [Discord example](../../examples/messaging_discord) provides guild threads,
DMs and bounded attachment downloads in an isolated workspace.
The [Slack example](../../examples/messaging_slack) uses Socket Mode, native
markdown tables and the existing Rig transports.

## Status reactions

Each run has a controller and a `ReactionHook`, attached with `add_hook`. Hooks
observe completion and tool dispatch in real time. They enqueue updates without
awaiting a platform API. One worker serializes reaction operations, adds the new
status before removing the old one, and applies only the last pending state after
the debounce window. Text deltas reset stalled-progress timers at most once per
second. This text-progress reset extends OpenAB's controller behavior.

Done and error reflect final delivery and persistence outcomes. Success adds a
random mood emoji. Reaction API failures are logged at debug level and do not
fail the reply. `ChatConfig::reactions.remove_after_reply` defaults to false;
when enabled, cleanup removes status and mood after the configured hold without
holding the conversation lock. Adapters without reactions skip the worker and
retention delay entirely.
