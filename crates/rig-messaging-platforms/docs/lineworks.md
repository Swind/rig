# LINE WORKS

Enable `rig/messaging-lineworks` or the companion's `lineworks` feature.
Construct `LineWorksConfig` using the bot ID, bot secret, bot display name,
OAuth client ID and secret, service-account ID, and RSA private key.
Use `https://www.worksapis.com/v1.0` for `api_base` and
`https://auth.worksmobile.com/oauth2/v2.0/token` for `token_url`.
The service account needs `bot.message,bot.read` scopes.
Register the gateway HTTPS callback URL in the Developer Console.

Callbacks require a valid raw-body `X-WORKS-Signature` and matching
`X-WORKS-BotId`. Sender identity and domain identity come from the signed
callback. Groups require a boundary-delimited `@BotName` mention. DMs route
through the user endpoint. Callback IDs combine the issued time and signed
body digest because the callback protocol exposes no message resource ID.

Text delivery accepts 10,000 Unicode scalar values. OAuth refresh is
serialized; a rejected token is refreshed and retried once. Optional flexible
text bubbles fall back to text only on HTTP 400. Sends acknowledge HTTP 201.
LINE WORKS returns no message resource ID, so `send_final` and `send_text`
report accepted delivery without inventing an address; `send` returns
`ChatError::Unsupported`. Edit, delete, reactions, and threads are unsupported.

Media downloads occur after Gate admission. Configure exact HTTPS
`media_hosts`. The authenticated API attachment endpoint must return HTTP 302;
the target is checked against the exact HTTPS host policy and downloaded
without a bearer token. Further redirects and URL credentials are rejected.
The MIME type comes from the bounded download response.

Run offline checks with
`cargo nextest run --locked --profile local -p rig-messaging-platforms --features lineworks`.
Live verification needs a configured bot and callback and an explicitly
selected user or room.

Protocol references: [callbacks](https://developers.worksmobile.com/en/docs/bot-callback),
[message events](https://developers.worksmobile.com/en/docs/bot-callback-message),
[service-account JWT](https://developers.worksmobile.com/en/docs/auth-jwt), and
[channel delivery](https://developers.worksmobile.com/en/docs/bot-channel-message-send).

Inbound context copies `issuedTime` as the event timestamp. Callback events do
not supply a channel display name or structured mentions; sender names fall
back to the signed user ID.
