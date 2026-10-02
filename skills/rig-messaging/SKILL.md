---
name: rig-messaging
description: Integrate Rig messaging into a native Rust application using ChatRouter, conversation memory, shared inbound context, and platform adapters. Use when adding a chat bot, connecting a supported messaging platform, or implementing a custom transport with rig-messaging.
---

# Rig messaging

Connect platform ingress to `ChatRouter` and a Rig agent with conversation
memory. Use `rig-messaging` for common routing and outbound operations, and
`rig-messaging-platforms` for its authenticated platform integrations.

## Read only what the task needs

- For dependencies, first integration, or a runnable starting point, read
  [references/getting-started.md](references/getting-started.md). It explains
  how to reuse [assets/stdio-bot.rs](assets/stdio-bot.rs) in another project.
- For Slack, Discord, or a supported platform, read
  [references/platforms.md](references/platforms.md), then inspect only that
  platform's example and configuration in the selected Rig checkout.
- For a custom transport, event normalization, or sender context, read
  [references/custom-ingress.md](references/custom-ingress.md).

For deeper agent-runtime work or embedded retrieval, use `$rig-agent` or
`$rig-qdrant-edge` when those skills are installed. They are optional; ordinary
messaging setup is covered by this skill's own references.

## Integration rules

Locate the Rig checkout or dependency revision used by the target project.
These instructions describe this checkout's APIs; its manifest version does
not prove that the same code is published. Keep all Rig crates on one source
revision. Check the selected source before adapting signatures or feature names.

Configure the agent with `.memory(...)` or `.memory_handler(...)` before
constructing `ChatRouter`. Share one router across ingress using the same
agent and memory backend. The reply channel's session key groups history;
everyone sharing that channel/thread shares the conversation.

Keep original message identity separate from the reply destination. Apply
`router.allows` before thread creation, profile lookups, or media downloads;
`handle` checks admission again. Platform signatures and authentication must
be verified before constructing trusted ingress. Prefer the existing `Gateway`
for platforms that implement `Platform`.

Supply names when available and retain platform IDs. `MessageContext` carries
optional channel name, UTC timestamp, and mentioned users. The router renders
these as user content. Names are descriptive data; authorization, routing, and
memory identity use IDs. Leave unavailable context absent instead of inventing it.

Reuse existing adapters. Slack and Discord adapters currently live in example
applications, not exported platform modules. Do not invent SDK imports or
facade features for them. Messaging is native-only.

Validate the target project's selected features and a focused offline test.
Use mock providers or local transports for automated checks. Explain which
parts remain unverified without credentials. Live platform sends require the
user's explicit authorization and a designated account/channel.

## Use from another project

Copy this entire `rig-messaging` directory into that environment's skill
directory, for example `~/.codex/skills/rig-messaging`. Keep `references/` and
`assets/` with `SKILL.md`. Invoke `$rig-messaging` when asking an agent to add
messaging support. The supporting files are portable; platform source examples
are located through the chosen Rig checkout, not this skill's installation path.
