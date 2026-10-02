# Google Chat

Enable `rig/messaging-googlechat` or the companion's `googlechat` feature.
Register a Chat app and an HTTPS endpoint in the Google Cloud console.
Set `bot_id` to the app's `users/...` identity and `audience` to the exact
configured callback URL or project number. Missing authentication fails closed.

`GoogleChatVerification::Endpoint` verifies Google OIDC issuer, audience,
expiry, signature, `email_verified`, and the exact
`chat@system.gserviceaccount.com` email. `ProjectNumber` verifies the Chat
service account's issuer and signing keys with a numeric audience.
`WorkspaceAddon` verifies the configured service-account email returned by
Workspace add-ons `projects.getAuthorization`, with endpoint URL audience.
Signing keys come from pinned Google endpoints and token header URLs are ignored.

Outbound credentials support an application-managed static access token,
RSA service-account OAuth JWT exchange with `chat.bot` scope, or GCE metadata
identity followed by IAM Credentials impersonation of a distinct target
service account. Give the runtime identity Service Account Token Creator on
the target account. Metadata requests disable proxies and redirects and require
`Metadata-Flavor: Google`. Self impersonation fails closed.

Messages retain actual space, message, and thread resource names. Native
mention annotations and sender metadata drive Gate admission. Sends return
real resource IDs and edits and deletes use those IDs. Writes are serialized
and paced at one operation per second for each space. HTTP 429 delays subsequent
space writes using a bounded Retry-After value; delivery is not replayed.
Refreshable OAuth credentials get one refresh after HTTP 401. Static-token
rejection is returned to the caller.

The adapter exposes a conservative 4,000-scalar message limit to remain inside
Google's JSON byte limit. Bot reactions are unsupported. Uploaded Chat media
is downloaded after Gate admission with a total event byte limit from the
trusted Google Chat host. Drive attachments produce a textual authorization
notice because accessing them requires separate user authorization.

Run offline checks with
`cargo nextest run --locked --profile local -p rig-messaging-platforms --features googlechat`.
These checks use real signed JWT fixtures and local HTTP delivery. Live tests
require a configured Chat app, audience, credentials, and explicitly selected space.

References: [request verification](https://developers.google.com/workspace/chat/verify-requests-from-chat),
[add-on HTTP authentication](https://developers.google.com/workspace/add-ons/guides/alternate-runtimes),
[message creation](https://developers.google.com/workspace/chat/api/reference/rest/v1/spaces.messages/create),
and [media download](https://developers.google.com/workspace/chat/api/reference/rest/v1/media/download).

Inbound context copies the message creation time, space display name when
present, and annotated user mentions. Missing mention display names fall back
to the user's resource name.
