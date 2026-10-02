# Normalize a custom transport

Use `ChannelRef`, `MessageRef`, `Sender`, `MessageContext`, and `Inbound` from
`rig_messaging`. Inspect `crates/rig-messaging/src/types.rs` and `adapter.rs` in
the chosen checkout when implementing the boundary.

The following function creates one normalized DM. It is an example of domain
mapping, not a webhook verifier:

```rust
use rig_messaging::{ChannelRef, Inbound, MessageContext, MessageRef, Sender};

fn direct_message(
    message_id: String,
    room_id: String,
    user_id: String,
    display_name: Option<String>,
    text: String,
) -> Inbound {
    let channel = ChannelRef {
        platform: "custom".into(),
        scope_id: None,
        channel_id: room_id,
        thread_id: None,
    };
    Inbound {
        message: MessageRef { channel: channel.clone(), message_id },
        reply_channel: channel,
        sender: Sender {
            name: display_name.unwrap_or_else(|| user_id.clone()),
            id: user_id,
            is_bot: false,
        },
        context: MessageContext::default(),
        text,
        attachments: vec![],
        is_dm: true,
        is_thread: false,
        mentions_bot: false,
    }
}
```

Populate `scope_id` with workspace/guild/tenant identity where applicable, and
use real platform message IDs. A Discord thread is a channel ID; platforms
with nested threads use `thread_id`. Keep `message.channel` as the original
address for Gate and reactions, and set `reply_channel` to the actual response
destination. Do not use display names or hand-built concatenations as memory keys.

`context.channel_name` is the original channel's optional display name.
`context.sent_at` is `Option<chrono::DateTime<chrono::Utc>>`; normalize native
timestamps to UTC and leave missing or malformed optional timestamps absent.
`context.mentions` contains `Sender` values with IDs and available names. Parse
native structured mention data when possible; avoid guessing identity from text.

`Inbound::prompt_text()` renders platform, channel, sender, optional time and
mentions before the original body. It escapes CR/LF/tab in header values and
deduplicates mention IDs. Do not wrap the body with another sender header before
calling the router. It is already supplied as model user content, not a system
instruction or authorization claim.

## Outbound transport

Implement `ChatAdapter` for send, edit, delete, and reaction operations. Return
real message addresses from `send`. Platforms lacking final message addresses
can implement `send_final` and disable preview edits. Use `ChatError` and
`rig_core::wasm_compat::WasmBoxedFuture` as defined by the trait.

Set `supports_edit()` and `supports_reactions()` to false when unavailable.
Their defaults are true. Return `ChatError::Unsupported` from those unsupported
methods. Advertise a positive `message_limit()` measured in Unicode scalar
values; enforce a stricter platform byte/block limit in the adapter if needed.
The bundled stdout adapter demonstrates the capability flags and error types.

Pass an `Arc<dyn ChatAdapter>`, a normalized event, and the actual platform bot
user ID to `router.handle(adapter, inbound, bot_user_id).await`. The bot ID is
used to reject self messages. Check `router.allows(&inbound, bot_user_id)` before
expensive preparation or creating a reply thread. Gate is admission policy;
it does not authenticate a forged webhook.

Validate mapping, missing metadata, admission, session identity, and outbound
capability behavior using a mock agent and a local transport. The checkout's
`crates/rig-messaging/src/types/tests.rs` and `router/tests.rs` provide examples.
