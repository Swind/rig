#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
use axum::{Router, routing::post};
use std::sync::atomic::{AtomicUsize, Ordering};
fn config() -> GoogleChatConfig {
    GoogleChatConfig {
        bot_id: "users/bot".into(),
        audience: "https://example.test/hook".into(),
        verification: GoogleChatVerification::Endpoint,
        auth: GoogleChatAuth::StaticToken("access".into()),
        api_base: "https://chat.googleapis.com/v1".into(),
        media_limit: 1024,
    }
}
fn token(email: &str, audience: &str) -> Result<String, Error> {
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(include_bytes!("../lineworks/test-key.pem"))
        .map_err(|_| Error::Invalid("test key"))?;
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("test".into());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Invalid("test clock"))?
        .as_secs();
    jsonwebtoken::encode(&header,&json!({"iss":"https://accounts.google.com","aud":audience,"exp":now+3600,"email":email,"email_verified":true}),&key).map_err(|_|Error::Invalid("test token"))
}
async fn adapter() -> Result<GoogleChat, Error> {
    let adapter = GoogleChat::new(config())?;
    adapter
        .verifier
        .seed_keys(serde_json::from_str(include_str!("test-jwks.json"))?)
        .await?;
    Ok(adapter)
}
fn request(token: &str) -> Result<WebhookRequest, Error> {
    let mut headers = ::http::HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|_| Error::Invalid("test header"))?,
    );
    let body = json!({"type":"MESSAGE","space":{"name":"spaces/room","type":"ROOM"},"message":{"name":"spaces/room/messages/input","text":"<users/bot> hi","argumentText":"hi","threadReply":true,"sender":{"name":"users/alice","displayName":"Alice","type":"HUMAN"},"thread":{"name":"spaces/room/threads/topic"},"annotations":[{"userMention":{"user":{"name":"users/bot"}}}]}});
    Ok(WebhookRequest {
        method: Method::POST,
        headers,
        query: Default::default(),
        body: serde_json::to_vec(&body)?.into(),
    })
}
#[tokio::test]
async fn valid_signature_normalizes_and_wrong_signer_or_audience_fail() -> Result<(), Error> {
    let adapter = adapter().await?;
    let verified = token(CHAT_SIGNER, &adapter.config.audience)?;
    let response = adapter.receive(request(&verified)?).await?;
    let inbound = &response
        .events
        .first()
        .ok_or(Error::Invalid("test event"))?
        .inbound;
    assert_eq!(inbound.sender.id, "users/alice");
    assert_eq!(inbound.text, "hi");
    assert!(inbound.mentions_bot);
    assert!(inbound.is_thread);
    assert_eq!(
        inbound.reply_channel.thread_id.as_deref(),
        Some("spaces/room/threads/topic")
    );
    assert!(matches!(
        adapter
            .receive(request(&token(
                "evil@example.test",
                &adapter.config.audience
            )?)?)
            .await,
        Err(Error::Authentication)
    ));
    assert!(matches!(
        adapter
            .receive(request(&token(CHAT_SIGNER, "wrong")?)?)
            .await,
        Err(Error::Authentication)
    ));
    assert!(matches!(
        adapter.add_reaction(&inbound.message, "👍").await,
        Err(ChatError::Unsupported(_))
    ));
    Ok(())
}
#[tokio::test]
async fn real_resource_ids_send_patch_delete_and_bearer() -> Result<(), Box<dyn std::error::Error>>
{
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let app = Router::new()
        .route(
            "/spaces/room/messages",
            post(
                move |headers: ::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
                    let count = count.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer access");
                        assert_eq!(body["text"], "hello");
                        count.fetch_add(1, Ordering::Relaxed);
                        axum::Json(json!({"name":"spaces/room/messages/sent"}))
                    }
                },
            ),
        )
        .route(
            "/spaces/room/messages/sent",
            axum::routing::patch(|| async {
                axum::Json(json!({"name":"spaces/room/messages/sent"}))
            })
            .delete(|| async { StatusCode::NO_CONTENT }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let mut cfg = config();
    cfg.api_base = format!("http://{address}");
    let adapter = GoogleChat::new(cfg)?;
    let channel = ChannelRef {
        platform: "googlechat".into(),
        scope_id: None,
        channel_id: "spaces/room".into(),
        thread_id: None,
    };
    let mut invalid = channel.clone();
    invalid.platform = "other".into();
    assert!(adapter.send(&invalid, "hello").await.is_err());
    invalid = channel.clone();
    invalid.thread_id = Some("spaces/other/threads/topic".into());
    assert!(adapter.send(&invalid, "hello").await.is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let message = adapter.send(&channel, "hello").await?;
    assert_eq!(message.message_id, "spaces/room/messages/sent");
    adapter.edit(&message, "edited").await?;
    adapter.delete(&message).await?;
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    server.abort();
    Ok(())
}
#[tokio::test]
async fn metadata_impersonation_is_distinct_and_uses_bounded_scoped_token()
-> Result<(), Box<dyn std::error::Error>> {
    let target = "chat-app@project.iam.gserviceaccount.com";
    let app = Router::new()
        .route("/computeMetadata/v1/instance/service-accounts/default/email",axum::routing::get(|headers: ::http::HeaderMap|async move {
            assert_eq!(headers["metadata-flavor"],"Google");
            ([("Metadata-Flavor","Google")],"runtime@project.iam.gserviceaccount.com")
        }))
        .route("/computeMetadata/v1/instance/service-accounts/default/token",axum::routing::get(||async {([( "Metadata-Flavor","Google")],axum::Json(json!({"access_token":"metadata-access"})))}))
        .route("/projects/-/serviceAccounts/chat-app@project.iam.gserviceaccount.com:generateAccessToken",post(|headers: ::http::HeaderMap,axum::Json(body):axum::Json<Value>|async move {
            assert_eq!(headers["authorization"],"Bearer metadata-access");assert_eq!(body["scope"],json!([SCOPE]));
            axum::Json(json!({"accessToken":"impersonated","expireTime":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let mut cfg = config();
    cfg.auth = GoogleChatAuth::Impersonation {
        target_service_account: target.into(),
    };
    let mut adapter = GoogleChat::new(cfg)?;
    adapter.metadata_base = format!("http://{address}");
    adapter.iam_base = format!("http://{address}");
    assert_eq!(adapter.token().await?, "impersonated");
    assert!(
        adapter
            .impersonated_token("runtime@project.iam.gserviceaccount.com")
            .await
            .is_err()
    );
    server.abort();
    Ok(())
}
