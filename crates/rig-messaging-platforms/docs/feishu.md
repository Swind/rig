# Feishu and Lark

`Feishu::connect(Config)` obtains a tenant access token and resolves the bot's
verified open ID. `Domain::Lark` selects `open.larksuite.com`; the default selects
`open.feishu.cn`. Both use the platform identifier `feishu` and the application ID
as their routing scope. Configure a self-built application with bot capabilities,
publish it, and subscribe to `im.message.receive_v1`.

For webhooks, configure `verification_token` and `encrypt_key` and expose a public
HTTPS callback through `Gateway::route`. The adapter verifies the original body
signature, a five-minute timestamp window, event token and application ID. It
decrypts AES-CBC event envelopes. The initial URL challenge is authenticated by
its verification token, following the official SDK's setup protocol. Missing
verification configuration fails closed.

For long connections, no public callback is required. Select long connection
delivery in the application's event configuration, then call `run_websocket` with
a bounded `mpsc::Sender<Incoming>` and a shutdown receiver. Send each received event
through `Gateway::dispatch_event`. The client bootstraps its endpoint with app
credentials, validates the returned WSS host, sends protobuf heartbeats, combines
bounded fragments, acknowledges events and reconnects. Events enter the queue after
their acknowledgement is written. Silent peers are disconnected after twice the
heartbeat interval plus the request timeout. A full event queue produces
a failed acknowledgement. Deduplication and admission belong to the Gateway.

```no_run
use std::sync::Arc;
use rig_messaging_platforms::{Gateway, feishu::{Config, Domain, Feishu}};
use tokio::sync::{mpsc, watch};

# async fn example(gateway: Arc<Gateway>) -> Result<(), Box<dyn std::error::Error>> {
let mut config = Config::new(std::env::var("FEISHU_APP_ID")?,
    std::env::var("FEISHU_APP_SECRET")?);
config.domain = Domain::Lark;
let bot = Feishu::connect(config).await?;
let (sender, mut receiver) = mpsc::channel(64);
let (_shutdown, shutdown) = watch::channel(false);
tokio::spawn(async move { let _ = bot.run_websocket(sender, shutdown).await; });
while let Some(event) = receiver.recv().await {
    gateway.dispatch_event(event).await?;
}
# Ok(()) }
```

Direct messages and groups use stable chat and sender open IDs. Replies to native
threads use the original root message and `reply_in_thread=true`. Bot mention
placeholders are removed from user text; other mentions retain readable names.
`prepare` downloads media only after admission. Supported inbound media are images,
post images, text files and audio. Limits are five files, 10 MiB per image,
512 KiB per text file, 25 MiB per audio file and 25 MiB total. Images are resized to
1200 pixels and encoded as JPEG; GIF bytes are preserved. Audio transcription is
an application responsibility.

The default `Delivery::Card` uses CardKit JSON 2.0 Markdown cards. Every preview
updates the full accumulated text with an increasing sequence. Final delivery
replaces the full card with a static card so Markdown tables are rendered again
and the cursor stops. Final messages sent without a preview are static cards.
`Delivery::Post` and `Delivery::Text` use their native message formats; previews
reserve two edits for final delivery before the twenty-edit server limit. A failed
final edit follows the shared router's delete-and-replace recovery. API failures
are reported, including rate limits; they are never treated as accepted sends.
An explicit rich-content rejection falls back to a post, then plain text if that
post is rejected. Network failures, server failures and rate limits do not trigger
this fallback because an uncertain retry could duplicate a delivered message.

Text is limited to 4000 Unicode scalar values per outgoing message. Message IDs
are the IDs returned by the service. Delete uses message recall. Reaction removal
lists reactions and deletes only this bot's matching reactions. Tenant tokens
are cached across clones; an expired-token response triggers one refresh and retry.
Each client retains at most 1024 card entries and edit counters. Finished card
entries may be evicted; later edits to an evicted card return an explicit error.

Outbound file uploads, native typing indicators, bot-to-bot event delivery and
native slash commands are not provided. Publishing the application, assigning its
permissions and choosing the bot's availability scope require administrator
configuration. No credentialed live messages are sent by the offline tests.

Official protocol references: [message APIs](https://open.feishu.cn/document/server-docs/im-v1/message/create),
[CardKit APIs](https://open.feishu.cn/document/cardkit-v1/card/create), and the
[official long connection SDK](https://github.com/larksuite/oapi-sdk-python/blob/v2_main/lark_oapi/ws/client.py).

Inbound context copies the message creation time and structured mentions with
their supplied user IDs and names. Events do not supply a chat display name;
missing mention names fall back to the supplied user ID.

```sh
cargo nextest run --locked --profile local -p rig-messaging-platforms --features feishu feishu
```
