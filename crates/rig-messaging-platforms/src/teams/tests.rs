#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, StatusCode};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

fn adapter() -> Result<Teams, Error> {
    Teams::new(
        TeamsConfig {
            app_id: "app".into(),
            app_secret: "secret".into(),
            tenant_id: "tenant".into(),
            oauth_endpoint: String::new(),
            jwks_url: "https://login.botframework.com/v1/.well-known/keys".into(),
            allowed_tenants: vec!["tenant".into()],
            service_hosts: vec!["smba.trafficmanager.net".into()],
            media_hosts: Vec::new(),
            media_limit: 1024,
        },
        Http::new(Duration::from_secs(2), 8192)?,
    )
}
fn activity() -> Value {
    json!({"type":"message","id":"original","serviceUrl":"https://smba.trafficmanager.net/region/","channelId":"msteams","channelData":{"tenant":{"id":"tenant"}},"conversation":{"id":"conversation","conversationType":"channel"},"from":{"id":"user","name":"User"},"text":"<at>Rig</at> hello","entities":[{"type":"mention","mentioned":{"id":"28:app"},"text":"<at>Rig</at>"}]})
}

#[test]
fn normalization_preserves_thread_identity_and_native_mentions() -> Result<(), Error> {
    let bot = adapter()?;
    let event = bot
        .normalize(&activity())?
        .ok_or(Error::Invalid("fixture event"))?;
    assert_eq!(event.inbound.sender.id, "user");
    assert_eq!(event.inbound.text, "hello");
    assert_eq!(
        event.inbound.reply_channel.thread_id.as_deref(),
        Some("original")
    );
    assert_eq!(
        event.inbound.reply_channel.scope_id.as_deref(),
        Some("tenant")
    );
    assert!(event.inbound.mentions_bot);
    assert!(!event.inbound.is_thread);
    assert!(!event.inbound.is_dm);
    let mut denied = activity();
    denied["channelData"]["tenant"]["id"] = json!("other");
    assert!(matches!(bot.normalize(&denied), Err(Error::Authentication)));
    Ok(())
}

#[tokio::test]
async fn endpoint_policy_blocks_bearer_leak_and_escapes_ids() -> Result<(), Error> {
    let bot = adapter()?;
    for url in [
        "http://smba.trafficmanager.net/",
        "https://evil.test/",
        "https://secret@smba.trafficmanager.net/",
        "https://smba.trafficmanager.net:8443/",
    ] {
        assert!(bot.service_url(url).is_err());
    }
    let event = bot
        .normalize(&activity())?
        .ok_or(Error::Invalid("fixture event"))?;
    let inbound = bot.prepare_event(event).await?;
    let url = bot
        .route(&inbound.reply_channel, Some("id/with?query"))
        .await?;
    assert!(url.ends_with("/v3/conversations/conversation/activities/id%2Fwith%3Fquery"));
    assert!(!bot.supports_reactions());
    Ok(())
}

#[tokio::test]
async fn receive_requires_matching_signed_service_and_channel_endorsement() -> Result<(), Error> {
    let bot = adapter()?;
    let pem = include_bytes!("test-key.pem");
    let key = EncodingKey::from_rsa_pem(pem).map_err(|_| Error::Invalid("fixture signing key"))?;
    // Derive the public JWK from the checked-in synthetic fixture key.
    let public = fixture_public_key()?;
    bot.verifier.seed_keys(json!({"keys":[public]})).await?;
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("fixture".into());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Invalid("fixture time"))?
        .as_secs();
    let claims = json!({"iss":"https://api.botframework.com","aud":"app","exp":now+300,"nbf":now-5,"serviceurl":"https://smba.trafficmanager.net/region/"});
    let token = encode(&header, &claims, &key).map_err(|_| Error::Invalid("fixture JWT"))?;
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| Error::Invalid("fixture header"))?,
    );
    let request = WebhookRequest {
        method: Method::POST,
        headers: headers.clone(),
        query: HashMap::new(),
        body: Bytes::from(serde_json::to_vec(&activity())?),
    };
    let response = bot.receive(request).await?;
    assert_eq!(response.events.len(), 1);
    assert!(response.events[0].inbound.attachments.is_empty());
    let mut altered = activity();
    altered["serviceUrl"] = json!("https://evil.test/");
    assert!(matches!(
        bot.receive(WebhookRequest {
            method: Method::POST,
            headers: headers.clone(),
            query: HashMap::new(),
            body: Bytes::from(serde_json::to_vec(&altered)?)
        })
        .await,
        Err(Error::Authentication)
    ));
    let mut public = fixture_public_key()?;
    public["endorsements"] = json!(["other"]);
    bot.verifier.seed_keys(json!({"keys":[public]})).await?;
    assert!(matches!(
        bot.receive(WebhookRequest {
            method: Method::POST,
            headers,
            query: HashMap::new(),
            body: Bytes::from(serde_json::to_vec(&activity())?)
        })
        .await,
        Err(Error::Authentication)
    ));
    Ok(())
}

fn fixture_public_key() -> Result<Value, Error> {
    Ok(serde_json::from_str(include_str!("test-jwk.json"))?)
}

#[tokio::test]
async fn connector_operations_use_real_ids_and_refresh_on_401() -> Result<(), Error> {
    use axum::{
        Json, Router,
        extract::State,
        routing::{any, post},
    };
    use std::sync::Arc;
    type Calls = Arc<Mutex<Vec<(Method, String, Value)>>>;
    async fn token() -> Json<Value> {
        Json(json!({"access_token":"fixture-token","expires_in":3600}))
    }
    async fn handle(
        State(calls): State<Calls>,
        method: Method,
        uri: http::Uri,
        bytes: Bytes,
    ) -> (StatusCode, Json<Value>) {
        let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        let mut calls = calls.lock().await;
        calls.push((method, uri.path().into(), body));
        if calls.len() == 1 {
            (StatusCode::UNAUTHORIZED, Json(json!({})))
        } else {
            (StatusCode::OK, Json(json!({"id":"actual-id"})))
        }
    }
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| Error::Invalid("fixture listener"))?;
    let address = listener
        .local_addr()
        .map_err(|_| Error::Invalid("fixture address"))?;
    let server_calls = calls.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/token", post(token))
                .route("/{*path}", any(handle))
                .with_state(server_calls),
        )
        .await
    });
    let mut bot = adapter()?;
    bot.config.oauth_endpoint = format!("http://{address}/token");
    bot.connector_base = Some(
        Url::parse(&format!("http://{address}/"))
            .map_err(|_| Error::Invalid("fixture Connector URL"))?,
    );
    let event = bot
        .normalize(&activity())?
        .ok_or(Error::Invalid("fixture event"))?;
    let inbound = bot.prepare_event(event).await?;
    let message = bot
        .send(&inbound.reply_channel, "hello")
        .await
        .map_err(Error::Chat)?;
    assert_eq!(message.message_id, "actual-id");
    bot.edit(&message, "updated").await.map_err(Error::Chat)?;
    bot.delete(&message).await.map_err(Error::Chat)?;
    let calls = calls.lock().await;
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[1].0, Method::POST);
    assert_eq!(calls[1].2["replyToId"], "original");
    assert_eq!(calls[2].0, Method::PUT);
    assert_eq!(calls[2].2["text"], "updated");
    assert!(calls[2].1.ends_with("/actual-id"));
    assert_eq!(calls[3].0, Method::DELETE);
    server.abort();
    Ok(())
}
