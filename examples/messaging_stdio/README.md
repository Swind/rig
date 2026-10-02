# Messaging stdio harness

Set `OPENAI_API_KEY`, optionally set `OPENAI_MODEL`, and run:

```sh
printf 'My name is Alice.\nWhat is my name?\n' | cargo run -p messaging_stdio
```

Each line runs in order in one conversation with in-memory history. Replies print
once per chunk after completion. The adapter does not edit or react. EOF exits
after the last turn. For token display, use Rig's `ChatBotBuilder` integration.
The local harness has no external channel metadata, so its optional timestamp,
channel display name, and mention context remain absent.

The test uses a mock model and requires no API key:

```sh
cargo nextest run --locked --profile local -p messaging_stdio
```
