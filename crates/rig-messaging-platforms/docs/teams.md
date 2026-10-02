# Microsoft Teams

Enable `rig-messaging-platforms/teams`. Construct `Teams` with `TeamsConfig` and
`Http`, then register it with `Gateway`. Create an Azure Bot/Entra registration,
enable its Teams channel, configure a public HTTPS messaging endpoint and install
its Teams application package in the intended tenant/conversations.

`app_id` and `app_secret` are the bot registration credentials. `tenant_id`
is the OAuth authority tenant: use the registration tenant for single-tenant
bots, or `botframework.com` for an existing compatible multi-tenant bot.
An empty `oauth_endpoint` derives the authority's Microsoft token URL.
The token scope is `https://api.botframework.com/.default`.
Keep credentials outside committed files.

Pin `jwks_url` to `https://login.botframework.com/v1/.well-known/keys` for public
Bot Connector traffic. Inbound JWTs require RS256, valid issuer/audience/expiry,
a matching service URL claim and a signing key endorsing `msteams`. Keys are
cached and refreshed for rotation. JWT header URLs never choose signing keys.
Set `allowed_tenants` to restrict tenant admission. Emulator authentication and
sovereign-cloud issuer/scope variants are outside this adapter's contract.

Set `service_hosts` to exact Connector hosts for your deployment, such as
`smba.trafficmanager.net` and `smba.infra.teams.microsoft.com`. Signed service
URLs must also be HTTPS, omit URL credentials and use the default HTTPS port.
Verified conversation references are registered after Gate admission. IDs are
encoded as URL path segments. The bounded reference cache has capacity 4096;
application callers need an admitted inbound event before sending to a
conversation. Durable proactive-conversation storage belongs to the application.

Native mentions identify the bot, and its mention markup is removed from prompt
text. Sender and tenant identities are required. Channel roots need a mention;
existing replies preserve the original root activity ID and are thread inputs.
DMs and group chats preserve their own conversation address.

Text sends return the real Connector activity ID. `reply` quotes an original
activity; `send` preserves channel-thread reply routing. Updates use PUT and
deletes use DELETE against the activity resource. The adapter uses a conservative
4000-scalar limit to leave room for the Connector payload envelope. Bot reactions
return `ChatError::Unsupported`, and the reaction capability flag is false.
OAuth refresh is serialized; an explicit 401 refreshes and retries once. Network
failures and other HTTP errors propagate without resending.

Attachment-only input is supported. Inline media uses Connector bearer auth only
for the verified service host. File-download-info URLs use no Connector bearer;
their exact HTTPS hosts must be configured in `media_hosts`. Signed file query
parameters are retained. Admission precedes downloads, and all attachments share
a finite byte budget. Cards and arbitrary Graph/SharePoint resources needing
separate Graph authentication are outside this adapter's download contract.

Offline verification:

```sh
cargo nextest run --locked --profile local -p rig-messaging-platforms --features teams teams
```

The suite signs synthetic fixture JWTs, checks endorsements/service URL/tenant
rejection, normalizes native mentions and verifies local Connector HTTP
serialization and 401 recovery. Live installation and delivery require an
explicitly designated tenant/conversation and credentials.

Protocol sources: [Connector authentication](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication),
[Teams conversations](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/build-conversational-capability).

Inbound context copies the activity timestamp, mention entities with stable
mentioned IDs, and a channel or conversation name when that activity supplies
one. Missing mention names fall back to the mentioned ID.
