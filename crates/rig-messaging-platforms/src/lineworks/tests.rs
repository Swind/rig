#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
use axum::{Router, extract::State, routing::post};
use hmac::{Hmac, Mac};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn config() -> LineWorksConfig {
    LineWorksConfig {
        bot_id: "123".into(),
        bot_secret: "secret".into(),
        bot_name: "Helper".into(),
        client_id: "client".into(),
        client_secret: "client-secret".into(),
        service_account: "service".into(),
        private_key: include_str!("test-key.pem").into(),
        api_base: "https://www.worksapis.com/v1.0".into(),
        token_url: "https://auth.worksmobile.com/oauth2/v2.0/token".into(),
        media_limit: 1024,
        media_hosts: vec!["www.worksapis.com".into()],
        rich_messages: false,
    }
}
fn request(body: Value) -> Result<WebhookRequest, Error> {
    let body = serde_json::to_vec(&body)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").map_err(|_| Error::Authentication)?;
    mac.update(&body);
    let mut headers = ::http::HeaderMap::new();
    headers.insert(
        "x-works-signature",
        STANDARD
            .encode(mac.finalize().into_bytes())
            .parse()
            .map_err(|_| Error::Invalid("test signature"))?,
    );
    headers.insert("x-works-botid", ::http::HeaderValue::from_static("123"));
    Ok(WebhookRequest {
        method: Method::POST,
        headers,
        query: Default::default(),
        body: body.into(),
    })
}
#[tokio::test]
async fn verified_identity_and_mention_boundaries() -> Result<(), Error> {
    let adapter = LineWorks::new(config())?;
    let body = json!({"type":"message","source":{"userId":"alice","channelId":"room","domainId":5},"issuedTime":"2026-10-01T00:00:00Z","content":{"type":"text","text":"hello @Helper"}});
    let response = adapter.receive(request(body.clone())?).await?;
    let event = response
        .events
        .first()
        .ok_or(Error::Invalid("test event"))?;
    assert_eq!(event.inbound.sender.id, "alice");
    assert!(event.inbound.mentions_bot);
    assert!(!event.inbound.is_dm);
    assert_eq!(event.inbound.message.channel.scope_id.as_deref(), Some("5"));
    assert!(!mentioned("@HelperExtra", "Helper"));
    assert!(!mentioned("x@Helper", "Helper"));
    let mut bad = request(body)?;
    bad.body = bytes::Bytes::from_static(b"{}");
    assert!(matches!(
        adapter.receive(bad).await,
        Err(Error::Authentication)
    ));
    assert!(matches!(
        adapter.send(&event.inbound.reply_channel, "hello").await,
        Err(ChatError::Unsupported(_))
    ));
    Ok(())
}
#[tokio::test]
async fn oauth_send_uses_user_endpoint_and_refreshes_once() -> Result<(), Box<dyn std::error::Error>>
{
    let calls = Arc::new(AtomicUsize::new(0));
    let tokens = Arc::new(AtomicUsize::new(0));
    let token_count = tokens.clone();
    let app = Router::new().route("/token",post(move || {let count=token_count.clone();async move {count.fetch_add(1,Ordering::Relaxed);axum::Json(json!({"access_token":"access","expires_in":3600}))}}))
        .route("/bots/123/users/alice/messages",post(|State(calls):State<Arc<AtomicUsize>>,axum::Json(body):axum::Json<Value>| async move {
            assert_eq!(body["content"]["text"],"hello");
            if calls.fetch_add(1,Ordering::Relaxed)==0 {StatusCode::UNAUTHORIZED} else {StatusCode::CREATED}
        })).with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let mut cfg = config();
    cfg.api_base = format!("http://{address}");
    cfg.token_url = format!("http://{address}/token");
    let adapter = LineWorks::new(cfg)?;
    let channel = ChannelRef {
        platform: "lineworks".into(),
        scope_id: Some("5".into()),
        channel_id: "user:alice".into(),
        thread_id: None,
    };
    adapter.send_final(&channel, "hello").await?;
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(tokens.load(Ordering::Relaxed), 2);
    server.abort();
    Ok(())
}
#[tokio::test]
async fn attachment_redirect_rejects_untrusted_host_before_forwarding_credentials()
-> Result<(), Box<dyn std::error::Error>> {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let app = Router::new()
        .route(
            "/token",
            post(|| async { axum::Json(json!({"access_token":"access","expires_in":3600})) }),
        )
        .route(
            "/bots/123/attachments/file",
            axum::routing::get(move |headers: ::http::HeaderMap| {
                let calls = counter.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer access");
                    calls.fetch_add(1, Ordering::Relaxed);
                    (StatusCode::FOUND, [("location", "https://evil.test/file")])
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let mut cfg = config();
    cfg.api_base = format!("http://{address}");
    cfg.token_url = format!("http://{address}/token");
    let adapter = LineWorks::new(cfg)?;
    let event = json!({"type":"message","source":{"userId":"alice","domainId":5},"issuedTime":"2026-10-01T00:00:00Z","content":{"type":"image","fileId":"file"}});
    let response = adapter.receive(request(event)?).await?;
    let event = response
        .events
        .into_iter()
        .next()
        .ok_or(Error::Invalid("test event"))?;
    assert!(matches!(
        adapter.prepare(event).await,
        Err(Error::Invalid("untrusted attachment URL"))
    ));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    server.abort();
    Ok(())
}
