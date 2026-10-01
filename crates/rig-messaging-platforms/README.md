# rig-messaging-platforms

Native authenticated ingress and outbound adapters for `rig-messaging`.
Enable individual features and serve `Gateway::route` through Axum. The default
build contains shared transport, admission, and token caching without platform
protocol dependencies.

```rust,no_run
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
use std::time::Duration;
use rig_messaging_platforms::{Http, WebhookResponse};
let http = Http::new(Duration::from_secs(30), 25 * 1024 * 1024)?;
assert_eq!(WebhookResponse::ack().status.as_u16(), 200);
# Ok(()) }
```

| Feature | Transport | Delivery |
| --- | --- | --- |
| [`telegram`](docs/telegram.md) | Secret-authenticated webhook or long polling | Text/rich text, private drafts, forum topics, edits, deletes, reactions |
| [`line`](docs/line.md) | Signed webhook | One-use reply token, then push; actual message IDs |
| [`lineworks`](docs/lineworks.md) | Signed webhook, service-account OAuth | Flex or text; accepted final sends have no message ID |
| [`wecom`](docs/wecom.md) | Encrypted corporate callbacks | Direct user text and recall |
| [`teams`](docs/teams.md) | Bot Connector JWT webhook | Markdown, replies, update and delete |
| [`googlechat`](docs/googlechat.md) | Google JWT webhook | Space/thread messages, update and delete |
| [`feishu`](docs/feishu.md) | Signed/encrypted webhook or native WebSocket | Feishu/Lark text, posts, CardKit streaming, threads, edits, deletes, reactions |

The facade exposes `rig::messaging_platforms` with `messaging-platforms` and
individual `messaging-telegram`, `messaging-line`, `messaging-lineworks`,
`messaging-wecom`, `messaging-teams`, `messaging-googlechat`, and
`messaging-feishu` features. All messaging integrations are native only.

## Admission and delivery

Platforms verify the raw request before producing `Incoming` events. Gateway
applies the router Gate and bounded recent-message deduplication before media
preparation. Webhook acknowledgement schedules processing without waiting for
agent generation. `Platform::scope` provides per-event reply context;
`Gateway::with_dispatch` wraps runs in application context such as model
session headers. Polling and WebSocket transports feed verified events into
`Gateway::dispatch_event`.

HTTP bodies, downloaded media, and deadlines are bounded. Redirects are
disabled. Private downloads require exact trusted HTTPS hosts, and HTTP error
URLs are stripped because API URLs can contain tokens. Token refreshes are
serialized and failed refreshes are not cached. Platform limits and API errors
remain explicit; unsupported operations return `ChatError::Unsupported`.

Gateway deduplication and agent tasks are in memory. An acknowledgement does
not promise durable queuing, delivery after process termination, or exactly
once external effects. Application deployments that require those guarantees
must persist verified events before acknowledgement and provide a worker queue.

See the [core contract](../rig-messaging/CONTRACT.md),
[platform expansion design](../../rig-messaging-platforms-plan.md), and
[executable gateway example](../../examples/messaging_gateway/README.md).

RSA keys in test fixture directories are synthetic, unregistered test data.
