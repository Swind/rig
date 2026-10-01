# Messaging gateway

Run one authenticated bot with conversation memory and a Rig agent. The example
uses the same model configuration as the [Slack example](../messaging_slack):
`OPENAI_API_KEY` with optional `OPENAI_MODEL`, or `OPENCODE_GO_API_KEY`,
`OPENCODE_GO_MODEL`, and `OPENCODE_GO_ENDPOINT`. The Go endpoint selects OpenAI
Chat, Responses, or Anthropic Messages compatibility. Go requests carry the
reply conversation's stable session ID and a truthful application User-Agent.
Cached ModelsDev metadata bounds estimated context and output budgets.

```sh
MESSAGING_PLATFORM=telegram cargo run --locked -p messaging_gateway
```

Load credentials from the ignored local `.env` or process environment. Never
commit tokens or private keys. `MESSAGING_PLATFORM` selects `telegram`, `line`,
`lineworks`, `wecom`, `teams`, `googlechat`, `feishu`, or `lark`.

| Platform | Required configuration |
| --- | --- |
| Telegram | `TELEGRAM_BOT_TOKEN`, `TELEGRAM_BOT_ID`, `TELEGRAM_BOT_USERNAME`, `TELEGRAM_WEBHOOK_SECRET` |
| LINE | `LINE_CHANNEL_SECRET`, `LINE_CHANNEL_ACCESS_TOKEN`, `LINE_BOT_ID` |
| LINE WORKS | `LINEWORKS_BOT_ID`, `LINEWORKS_BOT_SECRET`, `LINEWORKS_BOT_NAME`, `LINEWORKS_CLIENT_ID`, `LINEWORKS_CLIENT_SECRET`, `LINEWORKS_SERVICE_ACCOUNT`, `LINEWORKS_PRIVATE_KEY_FILE` |
| WeCom | `WECOM_CORP_ID`, numeric `WECOM_AGENT_ID`, `WECOM_SECRET`, `WECOM_CALLBACK_TOKEN`, `WECOM_ENCODING_AES_KEY` |
| Teams | `TEAMS_APP_ID`, `TEAMS_APP_SECRET`, `TEAMS_TENANT_ID` |
| Google Chat | `GOOGLE_CHAT_BOT_ID`, `GOOGLE_CHAT_AUDIENCE`; outbound credential mode below |
| Feishu/Lark | `FEISHU_APP_ID`, `FEISHU_APP_SECRET`; webhook mode additionally `FEISHU_VERIFICATION_TOKEN`, `FEISHU_ENCRYPT_KEY` |

`MESSAGING_LISTEN` defaults to `127.0.0.1:3000`; `MESSAGING_WEBHOOK_PATH` defaults
to `/webhook`. For webhooks, expose that literal route through your HTTPS
reverse proxy and register its public URL with the platform. LINE and LINE
WORKS need their documented callback subscriptions; Teams and Google Chat
also require the audience and app configuration described in the platform docs.

Telegram defaults to webhook mode. Set `TELEGRAM_MODE=polling` after disabling
the bot webhook to use long polling. Offsets are in process memory. Feishu and
Lark default to native WebSocket mode; set `FEISHU_MODE=webhook` to use the HTTP
callback instead. Long connections deliver verified events through the same
Gateway admission and media pipeline. Private Telegram draft APIs remain
available on the adapter; this common router uses message editing for previews.
Fatal native connection failures stop the server and return an error. Ctrl-C
signals the native worker to close its connection before process exit.

Optional comma-separated `MESSAGING_ALLOWED_CHANNELS` and
`MESSAGING_ALLOWED_USERS` configure Gate. Empty lists impose no restriction;
self messages are rejected and group messages require mentions or threads.
`TEAMS_ALLOWED_TENANTS` additionally restricts Teams tenant metadata.
`LINEWORKS_MEDIA_HOSTS` and `TEAMS_MEDIA_HOSTS` explicitly trust private media
hosts. Use only the HTTPS hosts actually issued by your platform. Attachment
MIME types are empty by default; configure `MESSAGING_ATTACHMENT_MIME_TYPES`
for a model that accepts those formats. Go uses text-only input in this example.

The server acknowledges webhooks before generation finishes. Tasks, recent-ID
deduplication, polling offsets and conversation memory do not survive restarts.
If delivery must survive restart, put verified events in durable storage and
run a persistent worker instead of this demonstration application.

Protocol tests use local HTTP/WebSocket transports. Additional platform tokens,
registered callbacks, and designated recipients are needed for live acceptance.
This example has not been tested against a live account for the seven new
platforms.

Google Chat uses `GOOGLE_CHAT_VERIFICATION=endpoint` by default. `project`
selects a project-number audience; `addon` requires the exact
`GOOGLE_CHAT_ADDON_SIGNER` service-account email returned by the add-on setup.
For outbound requests, provide `GOOGLE_CHAT_ACCESS_TOKEN`, or
`GOOGLE_CHAT_SERVICE_ACCOUNT` and `GOOGLE_CHAT_PRIVATE_KEY_FILE`, or
`GOOGLE_CHAT_IMPERSONATE_ACCOUNT` for metadata credentials with IAM permission
to impersonate the distinct Chat service account. Select only the mode matching
the configured app. Refer to the [Google Chat adapter documentation](../../crates/rig-messaging-platforms/docs/googlechat.md).
