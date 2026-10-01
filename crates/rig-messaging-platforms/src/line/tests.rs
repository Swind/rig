#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;

fn adapter() -> Result<Line, Error> {
    Line::new(
        LineConfig {
            channel_secret: "signed".into(),
            channel_access_token: "fixture".into(),
            bot_id: "bot".into(),
            api_base: "https://api.line.me".into(),
            data_base: "https://api-data.line.me".into(),
            media_limit: 1024,
        },
        Http::new(Duration::from_secs(2), 1024)?,
    )
}

#[test]
fn group_identity_and_native_self_mention() -> Result<(), Error> {
    let bot = adapter()?;
    let event = bot.normalize(&json!({"type":"message","source":{"type":"group","groupId":"group","userId":"user"},"message":{"type":"text","id":"7","text":"hello","mention":{"mentionees":[{"isSelf":true}]}}}))?.ok_or(Error::Invalid("test event"))?;
    assert_eq!(event.inbound.message.channel.channel_id, "group");
    assert_eq!(event.inbound.sender.id, "user");
    assert!(event.inbound.mentions_bot);
    assert!(!event.inbound.is_dm);
    assert!(bot.normalize(&json!({"type":"message","source":{"type":"group","groupId":"group"},"message":{"type":"text","id":"7","text":"hello"}}))?.is_none());
    Ok(())
}

#[tokio::test]
async fn signed_receive_preserves_raw_body_and_defers_media() -> Result<(), Error> {
    let bot = adapter()?;
    let body = Bytes::from(serde_json::to_vec(
        &json!({"events":[{"type":"message","source":{"type":"user","userId":"user"},"replyToken":"token","message":{"type":"image","id":"7"}}]}),
    )?);
    let mut mac =
        Hmac::<Sha256>::new_from_slice(b"signed").map_err(|_| Error::Invalid("test HMAC"))?;
    mac.update(&body);
    let signature = STANDARD.encode(mac.finalize().into_bytes());
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "x-line-signature",
        http::HeaderValue::from_str(&signature).map_err(|_| Error::Invalid("test signature"))?,
    );
    let response = bot
        .receive(WebhookRequest {
            method: Method::POST,
            headers: headers.clone(),
            query: HashMap::new(),
            body: body.clone(),
        })
        .await?;
    assert_eq!(response.events.len(), 1);
    assert!(response.events[0].inbound.attachments.is_empty());
    assert!(matches!(
        bot.receive(WebhookRequest {
            method: Method::POST,
            headers,
            query: HashMap::new(),
            body: Bytes::from_static(b"{}")
        })
        .await,
        Err(Error::Authentication)
    ));
    Ok(())
}

#[tokio::test]
async fn unsupported_operations_return_errors() -> Result<(), Error> {
    let bot = adapter()?;
    let channel = bot.channel(&json!({"type":"user","userId":"user"}))?;
    let message = MessageRef {
        channel,
        message_id: "7".into(),
    };
    assert!(!bot.supports_edit());
    assert!(!bot.supports_reactions());
    assert!(matches!(
        bot.edit(&message, "text").await,
        Err(ChatError::Unsupported(_))
    ));
    assert!(matches!(
        bot.delete(&message).await,
        Err(ChatError::Unsupported(_))
    ));
    Ok(())
}

#[tokio::test]
async fn reply_fallback_batches_and_true_ids() -> Result<(), Error> {
    use axum::{Json, Router, extract::State, routing::post};
    use std::sync::Arc;
    type Calls = Arc<Mutex<Vec<(String, Value)>>>;
    async fn handle(
        State(calls): State<Calls>,
        uri: http::Uri,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        calls.lock().await.push((uri.path().into(), body.clone()));
        if uri.path().ends_with("reply") {
            let message = if body["replyToken"] == "bad-body" {
                "Invalid messages"
            } else {
                "Invalid reply token"
            };
            return (StatusCode::BAD_REQUEST, Json(json!({"message":message})));
        }
        let sent: Vec<Value> = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, _)| json!({"id":format!("id-{index}")}))
            .collect();
        (StatusCode::OK, Json(json!({"sentMessages":sent})))
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
                .route("/{*path}", post(handle))
                .with_state(server_calls),
        )
        .await
    });
    let mut bot = adapter()?;
    bot.config.api_base = format!("http://{address}");
    let channel = bot.channel(&json!({"type":"user","userId":"user"}))?;
    let context = |token: &str, age: u64, channel_key: String| {
        Arc::new(Mutex::new(Some(ReplyToken {
            token: token.into(),
            received: Instant::now() - Duration::from_secs(age),
            channel_key,
        })))
    };
    let token = context("one-use", 0, channel.session_key());
    let ids = REPLY_TOKEN
        .scope(token.clone(), bot.send_text(&channel, &"😀".repeat(25001)))
        .await?;
    assert_eq!(ids.len(), 6);
    assert_eq!(ids[0].message_id, "id-0");
    let recorded = calls.lock().await;
    assert_eq!(recorded.len(), 3);
    assert!(recorded[0].0.ends_with("reply"));
    assert_eq!(recorded[1].1["messages"].as_array().map(Vec::len), Some(5));
    assert_eq!(recorded[2].1["messages"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        recorded[1].1["messages"][0]["text"]
            .as_str()
            .map(|text| text.chars().count()),
        Some(5000)
    );
    assert!(token.lock().await.is_none());
    drop(recorded);
    REPLY_TOKEN
        .scope(
            context("expired-token", 60, channel.session_key()),
            bot.send_text(&channel, "expired"),
        )
        .await?;
    assert_eq!(calls.lock().await.len(), 4);
    assert!(calls.lock().await[3].0.ends_with("push"));
    assert!(matches!(
        REPLY_TOKEN
            .scope(
                context("bad-body", 0, channel.session_key()),
                bot.send_text(&channel, "bad")
            )
            .await,
        Err(Error::Status(StatusCode::BAD_REQUEST))
    ));
    assert_eq!(calls.lock().await.len(), 5);
    let other = context("other-token", 0, "other-channel".into());
    REPLY_TOKEN
        .scope(other.clone(), bot.send_text(&channel, "unrelated"))
        .await?;
    assert!(other.lock().await.is_some());
    server.abort();
    Ok(())
}
