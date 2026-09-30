# Discord messaging example

Set `DISCORD_BOT_TOKEN` and `OPENAI_API_KEY`. Enable the Message Content intent in
Discord's developer portal. The bot needs channel visibility, send messages,
create public threads, send messages in threads, read message history and add
reactions permissions. `OPENAI_MODEL` defaults to `gpt-4o-mini`.

```sh
cargo run --manifest-path examples/messaging_discord/Cargo.toml
```

Mention the bot in a guild text channel to start a thread. Follow-up messages in
threads and DMs need no mention. History is in memory and shared by everyone in
the same thread. Reactions attach to the original triggering message. Responses
suppress mentions. Attachments are bounded to 10 MiB total per input, checked
before downloading and while reading the response.

Optional comma-separated `DISCORD_ALLOWED_CHANNELS` and `DISCORD_ALLOWED_USERS`
restrict admission. Channel ids match original channels; list thread ids for
follow-ups when a channel allowlist is configured. Bot messages are ignored.
This package has its own workspace to isolate serenity's TLS dependencies.

## Verification

```sh
cargo test --locked --manifest-path examples/messaging_discord/Cargo.toml
cargo clippy --locked --manifest-path examples/messaging_discord/Cargo.toml --all-targets
```

For live acceptance, mention the bot, continue in its thread, request a reply over
2000 characters and a markdown table, and ask it to use `count_characters`.
Observe queued, thinking, tool and done reactions on the triggering messages.
Intermediate reactions may debounce away on fast runs. Check DMs and a rejected
user/channel. Rejected input must create no thread and download no attachments.
