# LINE

Enable `rig-messaging-platforms/line`. Construct `Line` from `LineConfig` and
`Http`, then register it with `Gateway`. Use `https://api.line.me` as `api_base`
and `https://api-data.line.me` as `data_base`.

Create a LINE Official Account and Messaging API channel. Configure its channel
secret, channel access token and verified bot user ID. Register a public HTTPS
callback and enable webhooks. Disable automatic greeting/reply behavior when
the application should control replies. Keep credentials outside source control.

The adapter verifies HMAC-SHA256 against the original request bytes before
JSON parsing. DM, group and room addresses are separate. Group sender identity
must be supplied by LINE; sender-less group events are ignored. Native
`isSelf` mentions are preserved for Gate. Gate therefore rejects unmentioned
group media. Image and audio downloads happen after admission and use bearer
credentials only on the configured HTTPS data host, with redirects disabled
and a finite byte limit. External-provider media is rejected.

Reply tokens retain verified receive time and are scoped to the exact
triggering message by gateway dispatch. Each token is consumed at most once.
Tokens older than 55 seconds use push. An explicit `Invalid reply token`
response also permits push fallback. Other failures propagate without sending
a second copy. Push eligibility and account quotas remain LINE account rules.
Calls outside gateway dispatch use push directly.

`send_text` splits at 5000 Unicode scalars, sends batches of at most five
message objects and returns all actual reply/push message IDs. `ChatAdapter`
accepts one bounded chunk and returns its actual ID. A second chunk consumes
push quota after the first uses its reply token. Each admitted run holds one token; rejected callbacks allocate no shared reply-token state.

LINE does not expose bot text edits, deletion, reactions or conversation
threads through this adapter. These operations return `ChatError::Unsupported`,
and edit/reaction capability flags are false. Receive supports text, image and
audio messages. Files, video, stickers, postbacks, Flex output, loading-animation
requests and rich menus are outside this adapter's current receive/send contract.

Offline verification:

```sh
cargo nextest run --locked --profile local -p rig-messaging-platforms --features line line
```

Live verification requires an explicitly designated account/conversation and
credentials. No live sends are part of the offline suite.

Protocol reference: [LINE Messaging API](https://developers.line.biz/en/reference/messaging-api/).
