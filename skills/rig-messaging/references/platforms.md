# Choose a platform integration

Locate the selected Rig checkout first. Paths below are relative to its root,
not relative to the installed skill. Read only the selected platform's files.

| Platform | Direct companion feature | Implementation/configuration |
| --- | --- | --- |
| Telegram | `rig-messaging-platforms/telegram` | `docs/telegram.md`, `src/telegram.rs` |
| LINE | `rig-messaging-platforms/line` | `docs/line.md`, `src/line.rs` |
| LINE WORKS | `rig-messaging-platforms/lineworks` | `docs/lineworks.md`, `src/lineworks.rs` |
| WeCom | `rig-messaging-platforms/wecom` | `docs/wecom.md`, `src/wecom.rs` |
| Teams | `rig-messaging-platforms/teams` | `docs/teams.md`, `src/teams.rs` |
| Google Chat | `rig-messaging-platforms/googlechat` | `docs/googlechat.md`, `src/googlechat.rs` |
| Feishu/Lark | `rig-messaging-platforms/feishu` | `docs/feishu.md`, `src/feishu/` |

The implementation/configuration column is relative to
`crates/rig-messaging-platforms/`. Add that companion from the same dependency
source as `rig-messaging`, enabling only the feature needed. For the facade,
use `rig/messaging-telegram`, `messaging-line`, `messaging-lineworks`,
`messaging-wecom`, `messaging-teams`, `messaging-googlechat`, or
`messaging-feishu`, respectively. Import through `rig::messaging_platforms`.

## Authenticated gateway platforms

Use `examples/messaging_gateway/README.md` for environment configuration and
`examples/messaging_gateway/src/main.rs` for application startup. Follow its
`platforms::configured` helper in `examples/messaging_gateway/src/platforms.rs`
into the chosen platform constructor before writing new setup code. The example
enables all seven integrations; another project needs
only its selected feature. Environment variable names belong to the example,
not an automatic environment loader in the companion crate.

Construct the chosen `Platform` with its config and shared `Http`, then pass it
and an `Arc<ChatRouter>` to `Gateway::new(router, platform, body_limit,
dedup_limit)`. Limits must be positive. `Arc<Gateway>::route("/webhook")` yields
an Axum router. Use the example's polling/native WebSocket path when that mode
is selected instead of inventing a second event pipeline.

`Gateway` verifies requests through `Platform::receive`, deduplicates recent
events, and applies admission before media preparation and generation. Retain
raw request bytes for signature verification. Use `Platform::scope` through
gateway dispatch to preserve per-event reply credentials and metadata.

Telegram supports webhook or polling; disable the webhook before polling.
Feishu/Lark supports native WebSocket or webhook ingress. Other modes require
the platform-specific registered callback and authentication configuration.
Consult the platform's selected source documentation for credentials, audience,
signatures, and media host restrictions.

Example webhook acknowledgements precede generation. Deduplication, native
offsets, spawned tasks, and example memory are in process; use an application
durable queue/worker if delivery must survive restarts. Protocol tests are
offline evidence, not live account acceptance.

## Slack and Discord

Slack's Socket Mode transport and metadata cache are in
`examples/messaging_slack/src/slack.rs`; configuration and scopes are in that
example's README. Adapt this transport into the target application rather than
importing a nonexistent `rig_messaging_platforms::slack` module. Use the
example's bounded attachment downloads, event acknowledgements, and reconnect
behavior. Sender/channel enrichment requires `users:read` and the appropriate
conversation read scopes; failures fall back to IDs. Successful and failed
lookups are cached, and mention enrichment is bounded.

Discord's Serenity adapter is in `examples/messaging_discord/src/discord.rs`.
Its package has an isolated workspace and its own manifest/lockfile. Read its
README for credentials and intents. Preserve guild/channel/thread routing and
bounded attachment handling. It is not an exported companion module or a
`messaging-discord` facade feature.

## Context availability

All adapters use `Inbound.context` and the shared prompt renderer. Slack can
resolve names through its API; Discord supplies names and mentions through its
SDK. Telegram supplies chat names, timestamps, and structured user mentions;
Teams and Google Chat can supply channel/space names and mentioned users.
LINE and Feishu can supply mention IDs; LINE WORKS and WeCom do not provide
structured mention context in these adapters. Missing sender names use IDs.
Channel names unavailable in native payloads remain absent.

Read `crates/rig-messaging-platforms/README.md` for the exact availability
table. Do not promise that every platform yields every metadata field or add
profile APIs merely to fill optional fields.
