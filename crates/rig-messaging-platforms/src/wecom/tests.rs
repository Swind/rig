#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
use ::http::{HeaderMap, StatusCode};
use aes::cipher::BlockModeEncrypt;
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

fn adapter() -> Result<WeCom, Error> {
    WeCom::new(
        Config {
            corp_id: "corp".into(),
            agent_id: 42,
            secret: "secret".into(),
            callback_token: "callback".into(),
            encoding_aes_key: STANDARD.encode([1u8; 32]).trim_end_matches('=').into(),
        },
        Http::new(Duration::from_secs(2), 20 * 1024 * 1024)?,
    )
}
fn encrypted(adapter: &WeCom, xml: &str, corp: &str) -> Result<String, Error> {
    let mut plaintext = vec![7u8; 16];
    plaintext.extend_from_slice(&(xml.len() as u32).to_be_bytes());
    plaintext.extend_from_slice(xml.as_bytes());
    plaintext.extend_from_slice(corp.as_bytes());
    let padding = 32 - plaintext.len() % 32;
    plaintext.extend(std::iter::repeat_n(padding as u8, padding));
    let length = plaintext.len();
    let cipher = cbc::Encryptor::<aes::Aes256>::new_from_slices(&adapter.key, &adapter.key[..16])
        .map_err(|_| Error::Authentication)?;
    let bytes = cipher
        .encrypt_padded::<NoPadding>(&mut plaintext, length)
        .map_err(|_| Error::Authentication)?;
    Ok(STANDARD.encode(bytes))
}
fn request(adapter: &WeCom, value: &str, method: Method) -> Result<WebhookRequest, Error> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Authentication)?
        .as_secs()
        .to_string();
    let mut parts = [
        adapter.config.callback_token.as_str(),
        timestamp.as_str(),
        "nonce",
        value,
    ];
    parts.sort_unstable();
    let signature = format!("{:x}", Sha1::digest(parts.concat()));
    let mut query = HashMap::from([
        ("timestamp".into(), timestamp),
        ("nonce".into(), "nonce".into()),
        ("msg_signature".into(), signature),
    ]);
    if method == Method::GET {
        query.insert("echostr".into(), value.into());
    }
    Ok(WebhookRequest {
        method,
        headers: HeaderMap::new(),
        query,
        body: Bytes::from(format!("<xml><Encrypt><![CDATA[{value}]]></Encrypt></xml>")),
    })
}
#[tokio::test]
async fn encrypted_callback_identity_and_challenge() -> Result<(), Error> {
    let adapter = adapter()?;
    let xml = "<xml><ToUserName><![CDATA[corp]]></ToUserName><FromUserName>alice</FromUserName><MsgType>text</MsgType><Content><![CDATA[Hello & 世界]]></Content><MsgId>123</MsgId><AgentID>42</AgentID></xml>";
    let value = encrypted(&adapter, xml, "corp")?;
    let response = adapter
        .receive(request(&adapter, &value, Method::POST)?)
        .await?;
    assert_eq!(response.events.len(), 1);
    let inbound = &response.events[0].inbound;
    assert_eq!(inbound.text, "Hello & 世界");
    assert_eq!(inbound.sender.id, "alice");
    assert_eq!(inbound.message.message_id, "123");
    assert!(inbound.is_dm);
    assert!(!inbound.is_thread);
    assert_eq!(inbound.message.channel.scope_id.as_deref(), Some("corp"));
    let challenge = encrypted(&adapter, "challenge", "corp")?;
    assert_eq!(
        adapter
            .receive(request(&adapter, &challenge, Method::GET)?)
            .await?
            .body,
        Bytes::from_static(b"challenge")
    );
    Ok(())
}
#[tokio::test]
async fn rejects_forgery_stale_timestamp_and_wrong_corp() -> Result<(), Error> {
    let adapter = adapter()?;
    let value = encrypted(&adapter, "challenge", "corp")?;
    let mut forged = request(&adapter, &value, Method::GET)?;
    forged.query.insert("msg_signature".into(), "forged".into());
    assert!(matches!(
        adapter.receive(forged).await,
        Err(Error::Authentication)
    ));
    let mut stale = request(&adapter, &value, Method::GET)?;
    stale.query.insert("timestamp".into(), "1".into());
    assert!(matches!(
        adapter.receive(stale).await,
        Err(Error::Authentication)
    ));
    let other = encrypted(&adapter, "challenge", "other")?;
    assert!(matches!(
        adapter
            .receive(request(&adapter, &other, Method::GET)?)
            .await,
        Err(Error::Authentication)
    ));
    assert!(adapter.decrypt("AA==").is_err());
    Ok(())
}
#[tokio::test]
async fn rest_delivery_refresh_actual_id_recall_and_media() -> Result<(), Box<dyn std::error::Error>>
{
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captures = requests.clone();
    async fn send(
        State(captures): State<Arc<Mutex<Vec<Value>>>>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut requests = captures.lock().await;
        requests.push(body);
        if requests.len() == 1 {
            Json(json!({"errcode":42001}))
        } else {
            Json(json!({"errcode":0,"msgid":"real-id"}))
        }
    }
    let routes = Router::new()
        .route(
            "/cgi-bin/gettoken",
            get(|| async { Json(json!({"errcode":0,"access_token":"token","expires_in":7200})) }),
        )
        .route("/cgi-bin/message/send", post(send))
        .route(
            "/cgi-bin/message/recall",
            post(|| async { Json(json!({"errcode":0})) }),
        )
        .route(
            "/cgi-bin/media/get",
            get(|| async { (StatusCode::OK, "media") }),
        )
        .with_state(captures);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let mut adapter = adapter()?;
    adapter.base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, routes).await });
    let channel = ChannelRef {
        platform: "wecom".into(),
        scope_id: Some("corp".into()),
        channel_id: "alice".into(),
        thread_id: None,
    };
    let message = adapter.send(&channel, "hello").await?;
    assert_eq!(message.message_id, "real-id");
    assert_eq!(requests.lock().await.len(), 2);
    assert_eq!(
        requests.lock().await[1],
        json!({"touser":"alice","msgtype":"text","agentid":42,"text":{"content":"hello"}})
    );
    adapter.delete(&message).await?;
    assert_eq!(
        adapter.media("media-id", 1024).await?,
        Bytes::from_static(b"media")
    );
    assert!(!adapter.supports_edit());
    assert!(!adapter.supports_reactions());
    assert!(matches!(
        adapter.edit(&message, "x").await,
        Err(ChatError::Unsupported(_))
    ));
    assert!(adapter.send(&channel, &"😀".repeat(513)).await.is_err());
    let mut multicast = channel;
    multicast.channel_id = "alice|bob".into();
    assert!(adapter.send(&multicast, "x").await.is_err());
    server.abort();
    Ok(())
}
#[test]
fn image_types_are_preserved_and_unknown_formats_rejected() -> Result<(), Error> {
    assert_eq!(image_mime(b"\x89PNG\r\n\x1a\nrest")?, "image/png");
    assert_eq!(image_mime(b"GIF89a")?, "image/gif");
    assert_eq!(image_mime(b"\xff\xd8\xffrest")?, "image/jpeg");
    assert_eq!(image_mime(b"RIFFxxxxWEBP")?, "image/webp");
    assert!(image_mime(b"not an image").is_err());
    Ok(())
}
