# Telegram

Enable `rig-messaging-platforms/telegram`. Construct `Telegram` from
`TelegramConfig` and `Http`, then register it with `Gateway`.
Configure `api_base` as `https://api.telegram.org`; choose a bounded media limit
and an HTTP deadline longer than the polling timeout when using long polling.

Create a bot with BotFather. Set `bot_id` and `bot_username` from `getMe`.
Configure a public HTTPS webhook with `setWebhook`, passing the same
`secret_token` used in `webhook_secret`. Requests without that secret fail
before parsing. Keep the token and webhook secret outside committed config.
The adapter does not trust client-supplied source-IP headers.

For long polling, remove the webhook first. Call `Telegram::poll(offset, timeout)`
and pass the returned events through gateway admission and dispatch. Advance
and persist the returned offset after dispatch. Telegram retains updates for
at most 24 hours. Polling and webhooks cannot consume updates simultaneously.

Inbound context includes a supplied chat title or username and the message
timestamp. Structured `text_mention` entities include mentioned users; ordinary
username mentions do not identify a user in the payload. Missing display names
fall back to platform IDs.

Messages preserve chat IDs and forum topic IDs. Mentions use Telegram UTF-16
entity offsets. Photos, documents, voice and audio are downloaded in `prepare`
after Gate admission. Downloads require HTTPS from the configured API host,
reject redirects and enforce metadata and actual byte limits.

Text sends return real message IDs, and support edits, deletes and bot reactions.
The router splits at 4096 Unicode scalars. Telegram permits one bot reaction;
adding replaces it. Removing an older reaction preserves its newer replacement.
`🆗` maps to `👍`. Telegram may reject unavailable reaction emoji or insufficient
permissions. `create_topic` requires forum-topic permissions.

When `rich_messages` is enabled, table/math content uses native rich messages;
final router edits also carry native rich content. Explicit API rejection can
fall back to plain delivery. Network failures never trigger a second send.
`send_draft` accepts positive private-chat IDs and nonzero draft IDs, with a
32768-scalar maximum. Drafts are ephemeral; send the final reply separately.
Router previews use persistent messages and real IDs rather than draft IDs.

This integration handles `message` updates. Callback queries, inline queries,
channel posts, edited incoming messages, video, stickers and generation-stop
updates are outside its receive contract. Model transcription, command routing
and scheduled jobs belong to the application.

Offline verification:

```sh
cargo nextest run --locked --profile local -p rig-messaging-platforms --features telegram telegram
```

Live verification requires an explicitly designated bot/chat and credentials.
No live sends are part of the offline suite.

Protocol reference: [Telegram Bot API](https://core.telegram.org/bots/api).
