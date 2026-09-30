# Slack messaging example

Set `SLACK_BOT_TOKEN`, `SLACK_APP_TOKEN` and `OPENAI_API_KEY`.
`OPENAI_MODEL` defaults to `gpt-4o-mini`. Enable Socket Mode and create an
app-level token with `connections:write`. Install a bot with `chat:write`,
`reactions:write`, `channels:history`, `groups:history`, `im:history` and
`files:read` scopes. Subscribe to `message.channels`, `message.groups` and
`message.im`. Invite the bot to the channels it should handle.

```sh
cargo run --locked -p messaging_slack
```

A channel mention starts a reply thread. Follow-ups in threads and DMs need no
mention. Conversation history is in memory and shared by everyone in the same
thread. Reactions attach to the original input, including mentions outside a
thread. Sender names use Slack user ids without an extra profile lookup.
Optional comma-separated `SLACK_ALLOWED_CHANNELS` and `SLACK_ALLOWED_USERS`
restrict original channel ids and sender ids. Bot messages are ignored.

Replies use Block Kit markdown blocks with native tables and an 11,900 character
limit. Workspaces rejecting blocks fall back to plain text. Private attachments
are downloaded with the bot token only from HTTPS Slack URLs, up to 10 MiB total
per input. Both response headers and streamed bytes enforce the limit. Failed,
oversized and unavailable downloads become text notes.

Socket envelopes are acknowledged before agent turns start. The connection
handles ping/pong, disconnect and reconnect, with bounded HTTP, handshake and
socket operations. The last 1,024 message identities are retained across
reconnects to suppress duplicate deliveries, including overlapping mention and
message events. This cache and history do not survive process restarts. Handler
arrival order does not guarantee turn order; each conversation serializes at
its router lock. This example handles one installed workspace.

## Verification

```sh
cargo nextest run --locked --profile local -p messaging_slack
cargo clippy --locked -p messaging_slack --all-targets
cargo build --locked -p messaging_slack
```

Tests use local HTTP and WebSocket fixtures, including real transport calls,
blocked-turn acknowledgements, rejected input, history, native tables and
Unicode splitting. They need no platform or provider credentials.

For live acceptance, mention the bot, follow up in the resulting thread, send a
DM, request a table and a reply longer than 11,900 characters, and upload a small
supported image. Check the original message's queued, thinking and done
reactions. Fast transitions may debounce away. Check rejected users/channels,
then restart the Socket Mode connection and confirm it resumes receiving events.

Protocol references: [Socket Mode](https://docs.slack.dev/apis/events-api/using-socket-mode/),
[markdown blocks](https://docs.slack.dev/reference/block-kit/blocks/markdown-block/).
