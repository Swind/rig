# Slack messaging example

Set `SLACK_BOT_TOKEN` and `SLACK_APP_TOKEN`. The example automatically loads
`.env`; exported environment variables take precedence. Select OpenAI with
`OPENAI_API_KEY` (`OPENAI_MODEL` defaults to `gpt-4o-mini`), or configure
OpenCode Go as below. Enable Socket Mode and create an
app-level token with `connections:write`. Install a bot with `chat:write`,
`reactions:write`, `channels:history`, `groups:history`, `im:history` and
`files:read` scopes. Subscribe to `message.channels`, `message.groups` and
`message.im`. Invite the bot to the channels it should handle.

```sh
cargo run --locked -p messaging_slack
```

## OpenCode Go

Set a bare model id and its full endpoint from the
[Go endpoint table](https://opencode.ai/docs/go/#endpoints). The endpoint selects
Chat Completions, Responses or Anthropic Messages. Model names alone do not
select the protocol, and a `/v1/models` listing does not provide this mapping.

```dotenv
SLACK_BOT_TOKEN=xoxb-your-bot-token
SLACK_APP_TOKEN=xapp-your-app-token
SLACK_ALLOWED_CHANNELS=C1234567890
OPENCODE_GO_API_KEY=your-go-key
OPENCODE_GO_MODEL=glm-5.3-flash
OPENCODE_GO_ENDPOINT=https://opencode.ai/zen/go/v1/chat/completions
```

Examples from the provider's endpoint table:

| Model id | Endpoint suffix |
| --- | --- |
| `glm-5.3-flash` | `/v1/chat/completions` |
| `qwen3.8-flash` | `/v1/messages` |
| `grok-4.7` | `/v1/responses` |

When `OPENCODE_GO_API_KEY` is set, Go takes precedence over OpenAI. All three Go
variables are required, and the model id must omit the `opencode-go/` prefix.
`OPENCODE_GO_MAX_TOKENS` defaults to 4096 and must be positive. Requests identify
the client and carry a stable `x-opencode-session` for each conversation,
including follow-ups and tool continuations. Go media formats are not enabled
by default, so attachments become text notes. Enable formats in `ChatConfig`
only after confirming the selected model accepts them.

At startup, Go model limits are fetched from the `opencode-go` provider record
in [models.dev](https://models.dev). Its `limit.context` and `limit.output`
fields supply the context window and output cap. The fetch has a ten second
timeout and a 16 MiB response limit. Missing or unavailable metadata leaves the
configured output limit in place. This uses Rig's reusable
`model::models_dev::ModelsDev::get(provider, model)` API and existing `ModelInfo`
fields. The example shares one catalog for the process lifetime. Queries for
other models reuse the same download. `ModelsDev` clones share their cache;
`invalidate().await` clears it for the next fetch. The cache is in memory and
does not survive a process restart. Failed fetches are retried on the next query.

Each prepared model request, including history, instructions and tools, caps
output to the model's output limit and estimated remaining context. Input is
estimated using serialized UTF-8 bytes plus a 4096-token margin. This estimate
can leave context unused and is not an exact tokenizer count. An exhausted
estimate fails the turn without deleting history. No history compaction is
performed.

## Conversation behavior

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
Unicode splitting. They need no platform or provider credentials. Provider tests cover all three
Go wire protocols, authentication and conversation headers using local SSE
fixtures. Actual Go credentials are required for live model validation.

To validate two real streamed model turns with conversation history using the
Go configuration in `.env`:

```sh
cargo test --locked -p messaging_slack live_go_streams -- --ignored --nocapture
```

### Real Slack tests

Set `SLACK_TEST_CHANNEL` to a dedicated channel also listed in
`SLACK_ALLOWED_CHANNELS`. Invite the bot and grant `reactions:read` in addition
to the scopes above. The tests load `.env`, use actual credentials and send
messages to that channel. Run them serially:

```sh
cargo test --locked -p messaging_slack live_slack -- --ignored --nocapture --test-threads=1
```

The lifecycle test sends, reads through `conversations.history`, edits, adds
and reads reactions, removes reactions, creates and reads a thread through
`conversations.replies`, then deletes its own messages. The Socket Mode test
receives and acknowledges the actual posted message event and deletes its
test message. It requires `message.channels` or `message.groups` for the chosen
channel type.

The layout test uses a deterministic model with the real Slack adapter to
verify native markdown tables and lossless splitting of a Unicode reply longer
than 11,900 characters. It reads the posted blocks and text, then removes its
test thread.

The model/router test sends two real Go model replies through the Slack adapter,
reads the resulting thread and verifies the done reaction. It leaves the model
conversation in the test channel and prints its link. Input events use a
synthetic test sender because the bot's own messages are intentionally rejected.
This tests ingress, conversation memory, streaming delivery and reactions;
it does not impersonate a human or replace a manual user-mention check.

For live acceptance, mention the bot, follow up in the resulting thread, send a
DM, request a table and a reply longer than 11,900 characters, and upload a small
supported image. Check the original message's queued, thinking and done
reactions. Fast transitions may debounce away. Check rejected users/channels,
then restart the Socket Mode connection and confirm it resumes receiving events.

Protocol references: [Socket Mode](https://docs.slack.dev/apis/events-api/using-socket-mode/),
[markdown blocks](https://docs.slack.dev/reference/block-kit/blocks/markdown-block/).
