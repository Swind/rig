#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
use axum::{Router, body::Bytes, extract::State, response::IntoResponse, routing::any};
use base64::{Engine, engine::general_purpose::STANDARD};
use http::{HeaderMap, StatusCode, Uri};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Default)]
pub(super) struct Fixture {
    pub(super) requests: Arc<Mutex<Vec<(Method, String, Value)>>>,
    reject_first: Arc<Mutex<bool>>,
    reject_rich: Arc<Mutex<Option<StatusCode>>>,
    pub(super) websocket_url: Arc<Mutex<Option<String>>>,
}

async fn api_fixture(
    State(state): State<Fixture>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let payload = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    let mut requests = state.requests.lock().await;
    requests.push((method, uri.to_string(), payload));
    if uri.path().ends_with("tenant_access_token/internal") {
        let number = requests
            .iter()
            .filter(|(_, path, _)| path.ends_with("tenant_access_token/internal"))
            .count();
        return axum::Json(
            json!({"code":0,"tenant_access_token":format!("token{number}"),"expire":7200}),
        )
        .into_response();
    }
    if uri.path() == "/callback/ws/endpoint" {
        if let Some(url) = state.websocket_url.lock().await.clone() {
            return axum::Json(
                json!({"code":0,"data":{"URL":url,"ClientConfig":{"PingInterval":1}}}),
            )
            .into_response();
        }
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if !headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("Bearer token"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if uri.path().ends_with("/bot/v3/info") {
        return axum::Json(json!({"code":0,"bot":{"open_id":"ou_bot"}})).into_response();
    }
    let reject = std::mem::take(&mut *state.reject_first.lock().await);
    if reject {
        return axum::Json(json!({"code":99991663})).into_response();
    }
    if (uri.path().ends_with("/cardkit/v1/cards")
        || requests
            .last()
            .is_some_and(|(_, _, body)| body["msg_type"] == "interactive"))
        && let Some(status) = *state.reject_rich.lock().await
    {
        return status.into_response();
    }
    if uri.path().ends_with("/resources/img_test") {
        let image = image::RgbImage::new(2, 2);
        let mut data = std::io::Cursor::new(Vec::new());
        if image::DynamicImage::ImageRgb8(image)
            .write_to(&mut data, image::ImageFormat::Png)
            .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        return data.into_inner().into_response();
    }
    let data = if uri.path().ends_with("/cardkit/v1/cards") {
        json!({"card_id":"card_test"})
    } else if uri.path().ends_with("/reactions") && uri.query().is_some() {
        json!({"items":[{"reaction_id":"reaction_test","operator":{"operator_id":{"open_id":"ou_bot"}}}],"has_more":false})
    } else {
        json!({"message_id":"om_reply"})
    };
    axum::Json(json!({"code":0,"data":data})).into_response()
}

pub(super) async fn fixture(
    delivery: Delivery,
) -> Result<(Feishu, Fixture, tokio::task::JoinHandle<()>), Box<dyn std::error::Error + Send + Sync>>
{
    let state = Fixture::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let app = Router::new()
        .fallback(any(api_fixture))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let mut config = Config::new("cli_test", "secret");
    config.delivery = delivery;
    config.encrypt_key = Some("encryption-key".into());
    config.verification_token = Some("verification-token".into());
    let bot = Feishu::connect_at(config, base).await?;
    Ok((bot, state, task))
}

fn channel() -> ChannelRef {
    ChannelRef {
        platform: "feishu".into(),
        scope_id: Some("cli_test".into()),
        channel_id: "oc_chat".into(),
        thread_id: None,
    }
}

pub(super) fn event() -> Value {
    json!({"schema":"2.0","header":{"app_id":"cli_test","event_type":"im.message.receive_v1","token":"verification-token"},
        "event":{"sender":{"sender_id":{"open_id":"ou_user"},"sender_type":"user"},
        "message":{"message_id":"om_input","chat_id":"oc_chat","chat_type":"group","message_type":"text",
        "content":json!({"text":"@_user_1 hello"}).to_string(),"mentions":[{"key":"@_user_1","id":{"open_id":"ou_bot"}}]}}})
}

fn signed(value: &Value) -> Result<WebhookRequest, Box<dyn std::error::Error + Send + Sync>> {
    let body = Bytes::from(serde_json::to_vec(value)?);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .to_string();
    let mut hash = Sha256::new();
    hash.update(timestamp.as_bytes());
    hash.update(b"nonce");
    hash.update(b"encryption-key");
    hash.update(&body);
    let signature = hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut headers = HeaderMap::new();
    headers.insert("x-lark-request-timestamp", timestamp.parse()?);
    headers.insert("x-lark-request-nonce", "nonce".parse()?);
    headers.insert("x-lark-signature", signature.parse()?);
    Ok(WebhookRequest {
        method: Method::POST,
        headers,
        query: HashMap::new(),
        body,
    })
}

#[tokio::test]
async fn authenticated_callback_preserves_identity_and_rejects_tampering() -> TestResult {
    let (bot, state, task) = fixture(Delivery::Card).await?;
    let response = bot.receive(signed(&event())?).await?;
    assert_eq!(response.events.len(), 1);
    let inbound = &response.events[0].inbound;
    assert_eq!(inbound.text, "hello");
    assert!(inbound.mentions_bot);
    assert!(!inbound.is_dm);
    assert_eq!(inbound.message.message_id, "om_input");
    assert_eq!(inbound.sender.id, "ou_user");
    assert_eq!(state.requests.lock().await.len(), 2);
    let mut request = signed(&event())?;
    request.body = Bytes::from_static(b"{}");
    assert!(bot.receive(request).await.is_err());
    let mut request = signed(&event())?;
    request.headers.remove("x-lark-signature");
    assert!(matches!(
        bot.receive(request).await,
        Err(Error::Authentication)
    ));
    let mut changed = event();
    changed["header"]["app_id"] = json!("cli_other");
    assert!(matches!(
        bot.receive(signed(&changed)?).await,
        Err(Error::Authentication)
    ));
    task.abort();
    Ok(())
}

#[tokio::test]
async fn encrypted_callbacks_and_token_challenge_are_verified() -> TestResult {
    let (bot, _, task) = fixture(Delivery::Text).await?;
    let key = Sha256::digest(b"encryption-key");
    let iv = [7u8; 16];
    let plaintext = serde_json::to_vec(&event())?;
    let mut buffer = vec![0u8; plaintext.len() + 16];
    buffer[..plaintext.len()].copy_from_slice(&plaintext);
    let encrypted = cbc::Encryptor::<aes::Aes256>::new_from_slices(&key, &iv)
        .map_err(|_| "cipher key")?
        .encrypt_padded::<Pkcs7>(&mut buffer, plaintext.len())
        .map_err(|_| "cipher padding")?;
    let encoded = STANDARD.encode([iv.as_slice(), encrypted].concat());
    assert_eq!(
        bot.receive(signed(&json!({"encrypt":encoded}))?)
            .await?
            .events
            .len(),
        1
    );
    let challenge =
        json!({"type":"url_verification","token":"verification-token","challenge":"answer"});
    let mut request = signed(&challenge)?;
    request.headers.clear();
    let response = bot.receive(request).await?;
    assert_eq!(
        serde_json::from_slice::<Value>(&response.body)?,
        json!({"challenge":"answer"})
    );
    assert!(response.events.is_empty());
    let bad = json!({"type":"url_verification","token":"wrong","challenge":"answer"});
    assert!(matches!(
        bot.receive(signed(&bad)?).await,
        Err(Error::Authentication)
    ));
    task.abort();
    Ok(())
}

#[tokio::test]
async fn native_cards_keep_actual_ids_sequences_full_content_and_static_final() -> TestResult {
    let (bot, state, task) = fixture(Delivery::Card).await?;
    let message = bot.send(&channel(), "…").await?;
    assert_eq!(message.message_id, "om_reply");
    bot.edit(&message, "hello").await?;
    bot.edit(&message, "hello world").await?;
    bot.edit_final(&message, "| a |\n| - |\n| b |").await?;
    bot.send_final(&channel(), "finished continuation").await?;
    let requests = state.requests.lock().await;
    let updates = requests
        .iter()
        .filter(|(_, path, _)| path.contains("/elements/"))
        .collect::<Vec<_>>();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[1].2["content"], "hello world");
    assert_eq!(updates[0].2["sequence"], 1);
    assert_eq!(updates[1].2["sequence"], 2);
    let final_request = requests
        .iter()
        .find(|(_, path, _)| path == "/open-apis/cardkit/v1/cards/card_test")
        .ok_or("missing card final")?;
    let final_card: Value = serde_json::from_str(required(&final_request.2, "/card/data")?)?;
    assert_eq!(final_request.2["sequence"], 3);
    assert_eq!(final_card["config"]["streaming_mode"], false);
    assert_eq!(
        final_card["body"]["elements"][0]["content"],
        "| a |\n| - |\n| b |"
    );
    let final_send = requests.last().ok_or("missing final send")?;
    let content: Value = serde_json::from_str(required(&final_send.2, "/content")?)?;
    assert_eq!(content["config"]["streaming_mode"], false);
    task.abort();
    Ok(())
}

#[tokio::test]
async fn text_post_thread_reaction_delete_and_token_refresh_use_real_endpoints() -> TestResult {
    let (bot, state, task) = fixture(Delivery::Post).await?;
    let mut thread = channel();
    thread.thread_id = Some("om_root".into());
    *state.reject_first.lock().await = true;
    let message = bot.send(&thread, "**bold**").await?;
    bot.edit(&message, "replacement").await?;
    bot.add_reaction(&message, "👍").await?;
    bot.remove_reaction(&message, "👍").await?;
    bot.delete(&message).await?;
    let requests = state.requests.lock().await;
    assert_eq!(
        requests
            .iter()
            .filter(|(_, path, _)| path.ends_with("tenant_access_token/internal"))
            .count(),
        2
    );
    let send = requests
        .iter()
        .find(|(_, path, _)| path.ends_with("om_root/reply"))
        .ok_or("missing thread send")?;
    assert_eq!(send.2["reply_in_thread"], true);
    assert_eq!(send.2["msg_type"], "post");
    assert!(
        requests
            .iter()
            .any(|(method, path, _)| *method == Method::DELETE
                && path.ends_with("/reactions/reaction_test"))
    );
    assert!(
        requests
            .iter()
            .any(|(method, path, _)| *method == Method::DELETE
                && path.ends_with("/messages/om_reply"))
    );
    task.abort();
    Ok(())
}

#[tokio::test]
async fn media_is_deferred_and_image_bytes_match_normalized_mime() -> TestResult {
    let (bot, state, task) = fixture(Delivery::Text).await?;
    let mut image = event();
    image["event"]["message"]["message_type"] = json!("image");
    image["event"]["message"]["content"] = json!(json!({"image_key":"img_test"}).to_string());
    let response = bot.receive(signed(&image)?).await?;
    assert_eq!(state.requests.lock().await.len(), 2);
    let incoming = response
        .events
        .into_iter()
        .next()
        .ok_or("missing image event")?;
    assert!(incoming.inbound.attachments.is_empty());
    let inbound = bot.prepare(incoming).await?;
    assert_eq!(inbound.attachments.len(), 1);
    assert_eq!(inbound.attachments[0].mime, "image/jpeg");
    assert!(
        matches!(&inbound.attachments[0].source,rig_messaging::AttachmentSource::Bytes(bytes) if bytes.starts_with(&[0xff,0xd8]))
    );
    assert_eq!(state.requests.lock().await.len(), 3);
    task.abort();
    Ok(())
}

#[test]
fn domain_and_identifier_boundaries() {
    assert_eq!(Domain::Lark.api_base(), "https://open.larksuite.com");
    assert!(segment("../tokens").is_err());
    assert!(segment("..").is_err());
    assert!(segment("om_valid").is_ok());
    assert_eq!(reaction_type("👍"), Some("THUMBSUP"));
    assert_eq!(reaction_type("unknown"), None);
}

#[tokio::test]
async fn rich_fallback_requires_an_explicit_rejection_and_keeps_editable_ids() -> TestResult {
    let (bot, state, task) = fixture(Delivery::Card).await?;
    *state.reject_rich.lock().await = Some(StatusCode::BAD_REQUEST);
    let message = bot.send(&channel(), "hello").await?;
    assert_eq!(message.message_id, "om_reply");
    bot.edit(&message, "preview").await?;
    bot.edit_final(&message, "final").await?;
    bot.send_final(&channel(), "continuation").await?;
    let requests = state.requests.lock().await;
    assert!(
        requests
            .iter()
            .any(|(_, _, body)| body["msg_type"] == "post")
    );
    let patch = requests
        .iter()
        .find(|(method, _, _)| *method == Method::PATCH)
        .ok_or("missing fallback edit")?;
    let content: Value = serde_json::from_str(required(&patch.2, "/content")?)?;
    assert_eq!(content["zh_cn"]["content"][0][0]["text"], "preview");
    drop(requests);
    *state.reject_rich.lock().await = Some(StatusCode::INTERNAL_SERVER_ERROR);
    let before = state.requests.lock().await.len();
    assert!(bot.send(&channel(), "uncertain delivery").await.is_err());
    assert_eq!(state.requests.lock().await.len(), before + 1);
    *state.reject_rich.lock().await = Some(StatusCode::TOO_MANY_REQUESTS);
    assert!(bot.send_final(&channel(), "rate limited").await.is_err());
    assert_eq!(state.requests.lock().await.len(), before + 2);
    task.abort();
    Ok(())
}
