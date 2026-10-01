#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
use std::{collections::HashMap, time::Duration};

fn adapter() -> Result<Telegram, Error> {
    Telegram::new(
        TelegramConfig {
            bot_token: "fixture".into(),
            bot_id: "42".into(),
            bot_username: "RigBot".into(),
            webhook_secret: "signed".into(),
            api_base: "https://api.telegram.org".into(),
            media_limit: 1024,
            rich_messages: false,
        },
        Http::new(Duration::from_secs(2), 1024)?,
    )
}

#[test]
fn utf16_mentions_handle_astral_characters() {
    assert_eq!(utf16_slice("😀 @RigBot", 3, 7), Some("@RigBot"));
    assert_eq!(utf16_slice("😀 @RigBot", 1, 1), None);
    assert_eq!(utf16_slice("😀", 0, 0), Some(""));
}

#[test]
fn topic_identity_and_mention_are_normalized() -> Result<(), Error> {
    let bot = adapter()?;
    let event = bot.normalize(&json!({"update_id":1,"message":{"message_id":7,"message_thread_id":9,"chat":{"id":-100,"type":"supergroup"},"from":{"id":8,"first_name":"Sender","is_bot":false},"text":"😀 @RigBot", "entities":[{"type":"mention","offset":3,"length":7}]}}))?.ok_or(Error::Invalid("test event"))?;
    assert_eq!(event.inbound.message.message_id, "7");
    assert_eq!(event.inbound.reply_channel.thread_id.as_deref(), Some("9"));
    assert!(event.inbound.mentions_bot);
    assert!(event.inbound.is_thread);
    assert!(!event.inbound.is_dm);
    Ok(())
}

#[tokio::test]
async fn authentication_precedes_json_parsing() -> Result<(), Error> {
    let bot = adapter()?;
    let request = WebhookRequest {
        method: Method::POST,
        headers: http::HeaderMap::new(),
        query: HashMap::new(),
        body: Bytes::from_static(b"invalid json"),
    };
    assert!(matches!(
        bot.receive(request).await,
        Err(Error::Authentication)
    ));
    Ok(())
}

#[tokio::test]
async fn receive_defers_media_downloads() -> Result<(), Error> {
    let bot = adapter()?;
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "x-telegram-bot-api-secret-token",
        http::HeaderValue::from_static("signed"),
    );
    let request = WebhookRequest {
        method: Method::POST,
        headers,
        query: HashMap::new(),
        body: Bytes::from(serde_json::to_vec(
            &json!({"message":{"message_id":1,"chat":{"id":8,"type":"private"},"from":{"id":8,"first_name":"Sender"},"photo":[{"file_id":"would-download","file_size":10}]}}),
        )?),
    };
    let result = bot.receive(request).await?;
    assert_eq!(result.events.len(), 1);
    assert!(result.events[0].inbound.attachments.is_empty());
    Ok(())
}

#[tokio::test]
async fn outbound_returns_ids_and_serializes_topics_edits_and_delete() -> Result<(), Error> {
    use axum::{Json, Router, extract::State, routing::post};
    use std::sync::Arc;
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::<(String, Value)>::new()));
    async fn handle(
        State(calls): State<Arc<tokio::sync::Mutex<Vec<(String, Value)>>>>,
        uri: http::Uri,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        calls.lock().await.push((uri.path().into(), body.clone()));
        if uri.path().ends_with("getUpdates") {
            return (
                StatusCode::OK,
                Json(
                    json!({"ok":true,"result":[{"update_id":8,"message":{"message_id":9,"chat":{"id":8,"type":"private"},"from":{"id":8,"first_name":"User"},"text":"poll"}}]}),
                ),
            );
        }
        if body["text"] == "bad markdown" && body["parse_mode"] == "Markdown" {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    json!({"ok":false,"error_code":400,"description":"Bad Request: cannot parse entities"}),
                ),
            );
        }
        (
            StatusCode::OK,
            Json(json!({"ok":true,"result":{"message_id":77,"message_thread_id":9}})),
        )
    }
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
    let channel = ChannelRef {
        platform: "telegram".into(),
        scope_id: Some("42".into()),
        channel_id: "-100".into(),
        thread_id: Some("9".into()),
    };
    let message = bot.send(&channel, "hello").await.map_err(Error::Chat)?;
    assert_eq!(message.message_id, "77");
    bot.edit(&message, "updated").await.map_err(Error::Chat)?;
    bot.delete(&message).await.map_err(Error::Chat)?;
    assert!(matches!(
        bot.send_draft(&channel, 1, "draft").await,
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        bot.create_topic(&channel, "topic")
            .await?
            .thread_id
            .as_deref(),
        Some("9")
    );
    bot.send(&channel, "bad markdown")
        .await
        .map_err(Error::Chat)?;
    let (offset, events) = bot.poll(5, 20).await?;
    assert_eq!(offset, 9);
    assert_eq!(events.len(), 1);
    assert!(events[0].inbound.is_dm);
    bot.add_reaction(&message, "👀")
        .await
        .map_err(Error::Chat)?;
    bot.add_reaction(&message, "🤔")
        .await
        .map_err(Error::Chat)?;
    bot.remove_reaction(&message, "👀")
        .await
        .map_err(Error::Chat)?;
    bot.remove_reaction(&message, "🤔")
        .await
        .map_err(Error::Chat)?;
    let mut private = channel.clone();
    private.channel_id = "8".into();
    private.thread_id = None;
    bot.send_draft(&private, 1, "draft").await?;
    bot.send_rich(&private, &"x".repeat(5000)).await?;
    bot.config.rich_messages = true;
    bot.edit_final(&message, "| heading | value |")
        .await
        .map_err(Error::Chat)?;
    assert!(matches!(
        bot.edit_final(&message, &"x".repeat(4097)).await,
        Err(ChatError::Platform(_))
    ));
    let calls = calls.lock().await;
    assert!(
        calls
            .iter()
            .any(|(path, body)| path.ends_with("sendMessage") && body["message_thread_id"] == 9)
    );
    assert!(
        calls
            .iter()
            .any(|(path, body)| path.ends_with("editMessageText") && body["message_id"] == 77)
    );
    assert!(calls.iter().any(|(path, body)| path.ends_with("getUpdates")
        && body["offset"] == 5
        && body["timeout"] == 20));
    assert_eq!(
        calls
            .iter()
            .filter(|(path, _)| path.ends_with("setMessageReaction"))
            .count(),
        3
    );
    assert!(
        calls
            .iter()
            .any(|(path, body)| path.ends_with("sendRichMessageDraft") && body["draft_id"] == 1)
    );
    assert!(
        calls
            .iter()
            .any(|(path, body)| path.ends_with("editMessageText")
                && body["rich_message"]["markdown"] == "| heading | value |")
    );
    server.abort();
    Ok(())
}
