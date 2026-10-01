# WeCom corporate application

Enable `wecom` and construct `wecom::WeCom` with a corporate application ID,
secret, callback token, and 43-character EncodingAESKey. This integration is
for direct user messages from corporate applications. It does not address
consumer WeChat or arbitrary WeCom groups.

Serve `Gateway::route` at the configured callback path. GET verification and
POST callbacks validate SHA1 signatures, five-minute timestamp freshness,
AES-256-CBC padding with WeCom's 32-byte convention, and the decrypted corporate
identity. Unsupported event types are acknowledged without invoking agents.
Recent message deduplication belongs to the shared gateway.

Text, image, and supported text files normalize to stable corporate/user
addresses. Media is fetched from the fixed corporate API after admission;
callback image URLs are never fetched. Access tokens are cached and rejected
tokens refresh once. Binary responses and text responses are bounded by the
configured HTTP limit, with additional 10 MiB image and 20 MiB file ceilings.
The caller must configure allowed attachment MIME types in `ChatConfig`.

Text sending returns real `msgid` values. Recall uses the message recall API.
Edits, reactions, and threads return `Unsupported`. The scalar message limit
is conservatively 512 so all Unicode messages remain below the 2048-byte text
ceiling. No provisional messages are sent because edits are unavailable.

Offline tests exercise authenticated encrypted callbacks, corporate mismatch,
stale requests, token rejection and refresh, actual send IDs, recall, bounded
media, and recipient validation. Live tests require a designated corporate
application and recipient; no live WeCom account has been tested here.
