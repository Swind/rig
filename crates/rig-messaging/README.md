# rig-messaging

Native messaging platform transports and conversation routing for Rig agents.
Platform ingress normalizes messages into `Inbound`. `ChatAdapter` provides
outbound send, edit, delete and reaction operations without platform SDKs.

Use the companion directly or enable the facade's `messaging` feature and import
`rig::messaging`. This crate supports native targets only.

Message references retain the original channel for reactions. `reply_channel`
selects the destination and conversation identity. Session keys encode all four
routing fields with lengths so platform identifiers cannot collide.
