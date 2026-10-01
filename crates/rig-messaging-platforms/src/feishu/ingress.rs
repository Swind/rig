use super::*;
use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http::StatusCode;
use rig_messaging::{Attachment, AttachmentSource, Sender};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

impl Feishu {
    pub(super) async fn webhook(&self, request: WebhookRequest) -> Result<WebhookResponse, Error> {
        if request.method != Method::POST {
            return Err(Error::Invalid("Feishu callbacks require POST"));
        }
        if request.body.len() > 1024 * 1024 {
            return Err(Error::TooLarge);
        }
        let token = self
            .config
            .verification_token
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(Error::Authentication)?;
        let key = self
            .config
            .encrypt_key
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(Error::Authentication)?;
        let envelope: Value = serde_json::from_slice(&request.body)?;
        let value = match envelope.get("encrypt").and_then(Value::as_str) {
            Some(encrypted) => decrypt(key, encrypted)?,
            None => envelope,
        };
        let event_token = value
            .pointer("/header/token")
            .or_else(|| value.get("token"))
            .and_then(Value::as_str)
            .ok_or(Error::Authentication)?;
        crate::auth::verify_secret(token.as_bytes(), event_token.as_bytes())?;
        let challenge = value.get("type").and_then(Value::as_str) == Some("url_verification");
        // The setup challenge is authenticated by its verification token; event callbacks also require the signed raw body.
        if !challenge || request.headers.contains_key("x-lark-signature") {
            let timestamp = header(&request, "x-lark-request-timestamp")?;
            let nonce = header(&request, "x-lark-request-nonce")?;
            let signature = header(&request, "x-lark-signature")?;
            let seconds = timestamp
                .parse::<u64>()
                .map_err(|_| Error::Authentication)?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| Error::Authentication)?
                .as_secs();
            if now.abs_diff(seconds) > 300 {
                return Err(Error::Authentication);
            }
            let mut hash = Sha256::new();
            hash.update(timestamp.as_bytes());
            hash.update(nonce.as_bytes());
            hash.update(key.as_bytes());
            hash.update(&request.body);
            let expected = hash
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            crate::auth::verify_secret(expected.as_bytes(), signature.as_bytes())?;
        }
        if challenge {
            let challenge = required(&value, "/challenge")?;
            return Ok(WebhookResponse {
                status: StatusCode::OK,
                content_type: "application/json",
                body: Bytes::from(serde_json::to_vec(&json!({"challenge":challenge}))?),
                events: Vec::new(),
            });
        }
        let mut response = WebhookResponse::ack();
        response.content_type = "application/json";
        response.body = Bytes::from_static(b"{}");
        if let Some(event) = self.normalize(&value)? {
            response.events.push(event);
        }
        Ok(response)
    }

    pub(super) fn normalize(&self, value: &Value) -> Result<Option<Incoming>, Error> {
        if value.pointer("/header/event_type").and_then(Value::as_str)
            != Some("im.message.receive_v1")
        {
            return Ok(None);
        }
        if required(value, "/header/app_id")? != self.config.app_id {
            return Err(Error::Authentication);
        }
        let message = value
            .pointer("/event/message")
            .ok_or(Error::Invalid("missing Feishu message"))?;
        let sender = required(value, "/event/sender/sender_id/open_id")?;
        let sender_type = required(value, "/event/sender/sender_type")?;
        let message_id = required(message, "/message_id")?;
        let chat_id = required(message, "/chat_id")?;
        segment(message_id)?;
        segment(chat_id)?;
        segment(sender)?;
        let kind = required(message, "/message_type")?;
        if !matches!(kind, "text" | "post" | "image" | "file" | "audio") {
            return Ok(None);
        }
        let content: Value = serde_json::from_str(required(message, "/content")?)?;
        let mut text = match kind {
            "text" => content
                .pointer("/text")
                .unwrap_or(&serde_json::Value::Null)
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            "post" => post_text(&content),
            _ => String::new(),
        };
        let mut mentions_bot = false;
        if let Some(mentions) = message
            .pointer("/mentions")
            .unwrap_or(&serde_json::Value::Null)
            .as_array()
        {
            for mention in mentions {
                let id = mention.pointer("/id/open_id").and_then(Value::as_str);
                let ours = id == Some(self.bot_id.as_str());
                mentions_bot |= ours;
                if let Some(key) = mention
                    .pointer("/key")
                    .unwrap_or(&serde_json::Value::Null)
                    .as_str()
                {
                    let replacement = if ours {
                        "".to_owned()
                    } else {
                        format!(
                            "@{}",
                            mention
                                .pointer("/name")
                                .unwrap_or(&serde_json::Value::Null)
                                .as_str()
                                .unwrap_or("user")
                        )
                    };
                    text = text.replace(key, &replacement);
                }
            }
        }
        let thread = message
            .pointer("/root_id")
            .unwrap_or(&serde_json::Value::Null)
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                message
                    .pointer("/parent_id")
                    .unwrap_or(&serde_json::Value::Null)
                    .as_str()
                    .filter(|s| !s.is_empty())
            })
            .map(str::to_owned);
        if let Some(thread) = &thread {
            segment(thread)?;
        }
        let channel = ChannelRef {
            platform: "feishu".into(),
            scope_id: Some(self.config.app_id.clone()),
            channel_id: chat_id.into(),
            thread_id: thread.clone(),
        };
        let inbound = Inbound {
            message: MessageRef {
                channel: channel.clone(),
                message_id: message_id.into(),
            },
            reply_channel: channel,
            sender: Sender {
                id: sender.into(),
                name: sender.into(),
                is_bot: sender_type == "app",
            },
            text: text.trim().into(),
            attachments: Vec::new(),
            is_dm: message
                .pointer("/chat_type")
                .unwrap_or(&serde_json::Value::Null)
                .as_str()
                == Some("p2p"),
            is_thread: thread.is_some(),
            mentions_bot,
        };
        Ok(Some(Incoming {
            inbound,
            payload: json!({"content":content,"message_type":kind}),
        }))
    }

    pub(super) async fn prepare_media(&self, mut incoming: Incoming) -> Result<Inbound, Error> {
        self.address(&incoming.inbound.message.channel)?;
        let message_id = &incoming.inbound.message.message_id;
        segment(message_id)?;
        let kind = required(&incoming.payload, "/message_type")?;
        let content = &incoming
            .payload
            .pointer("/content")
            .unwrap_or(&serde_json::Value::Null);
        let mut references = Vec::new();
        match kind {
            "image" => references.push((required(content, "/image_key")?.to_owned(), "image")),
            "file" | "audio" => {
                references.push((required(content, "/file_key")?.to_owned(), "file"))
            }
            "post" => collect_images(content, &mut references),
            _ => {}
        }
        if references.len() > 5 {
            return Err(Error::TooLarge);
        }
        let mut total = 0usize;
        for (key, resource_type) in references {
            segment(&key)?;
            let limit = match (resource_type, kind) {
                ("image", _) => 10 * 1024 * 1024,
                (_, "audio") => 25 * 1024 * 1024,
                _ => 512 * 1024,
            };
            let media = Http::new(self.config.timeout, limit)?;
            let url = format!(
                "{}/open-apis/im/v1/messages/{message_id}/resources/{key}?type={resource_type}",
                self.base
            );
            let filename = content
                .pointer("/file_name")
                .unwrap_or(&serde_json::Value::Null)
                .as_str()
                .unwrap_or(if resource_type == "image" {
                    "image"
                } else if kind == "audio" {
                    "audio.ogg"
                } else {
                    "file.txt"
                });
            let mime = if resource_type == "image" {
                "image/jpeg"
            } else {
                mime(filename, kind)?
            };
            let token = self.token().await?;
            let data = match media
                .execute(media.request(Method::GET, &url).bearer_auth(token))
                .await
            {
                Err(Error::Status(StatusCode::UNAUTHORIZED)) => {
                    self.tokens.invalidate().await;
                    media
                        .execute(
                            media
                                .request(Method::GET, &url)
                                .bearer_auth(self.token().await?),
                        )
                        .await?
                }
                result => result?,
            };
            total = total.checked_add(data.len()).ok_or(Error::TooLarge)?;
            if data.len() > limit || total > 25 * 1024 * 1024 {
                return Err(Error::TooLarge);
            }
            let (data, mime) = if resource_type == "image" {
                tokio::task::spawn_blocking(move || resize_image(data))
                    .await
                    .map_err(|_| Error::Invalid("Feishu image processing failed"))??
            } else {
                (data, mime)
            };
            incoming.inbound.attachments.push(Attachment {
                filename: filename.into(),
                mime: mime.into(),
                size: Some(data.len() as u64),
                source: AttachmentSource::Bytes(data),
            });
        }
        Ok(incoming.inbound)
    }
}

fn resize_image(data: Bytes) -> Result<(Bytes, &'static str), Error> {
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Ok((data, "image/gif"));
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .map_err(|_| Error::Invalid("invalid Feishu image"))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| Error::Invalid("invalid Feishu image"))?;
    let image = if image.width() > 1200 || image.height() > 1200 {
        image.resize(1200, 1200, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 75)
        .encode_image(&image.to_rgb8())
        .map_err(|_| Error::Invalid("Feishu image encoding failed"))?;
    Ok((Bytes::from(bytes), "image/jpeg"))
}

fn header<'a>(request: &'a WebhookRequest, name: &'static str) -> Result<&'a str, Error> {
    request
        .headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .ok_or(Error::Authentication)
}

fn decrypt(key: &str, encoded: &str) -> Result<Value, Error> {
    let mut bytes = STANDARD
        .decode(encoded)
        .map_err(|_| Error::Authentication)?;
    if bytes.len() < 32 {
        return Err(Error::Authentication);
    }
    let cipher_key = Sha256::digest(key.as_bytes());
    let (iv, data) = bytes.split_at_mut(16);
    let decryptor = cbc::Decryptor::<aes::Aes256>::new_from_slices(&cipher_key, iv)
        .map_err(|_| Error::Authentication)?;
    let plaintext = decryptor
        .decrypt_padded::<Pkcs7>(data)
        .map_err(|_| Error::Authentication)?;
    serde_json::from_slice(plaintext).map_err(|_| Error::Authentication)
}

fn post_text(value: &Value) -> String {
    if let Some(post) = value.get("content") {
        return rows_text(post);
    }
    value
        .as_object()
        .and_then(|o| o.values().find_map(|v| v.get("content")))
        .map(rows_text)
        .unwrap_or_default()
}

fn rows_text(rows: &Value) -> String {
    rows.as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row.as_array()
                        .map(|nodes| {
                            nodes
                                .iter()
                                .filter_map(|node| {
                                    node.get("text")
                                        .or_else(|| node.get("user_name"))
                                        .and_then(Value::as_str)
                                })
                                .collect::<String>()
                        })
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn collect_images(value: &Value, result: &mut Vec<(String, &'static str)>) {
    if let Some(key) = value.get("image_key").and_then(Value::as_str) {
        result.push((key.into(), "image"));
    }
    match value {
        Value::Array(values) => {
            for value in values {
                collect_images(value, result);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_images(value, result);
            }
        }
        _ => {}
    }
}

fn mime(filename: &str, kind: &str) -> Result<&'static str, Error> {
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if kind == "audio" {
        return Ok(match ext.as_str() {
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "m4a" => "audio/mp4",
            _ => "audio/ogg",
        });
    }
    match ext.as_str() {
        "txt" | "rs" | "py" | "js" | "ts" | "toml" | "yaml" | "yml" | "csv" | "log" | "sh"
        | "sql" | "html" | "css" => Ok("text/plain"),
        "md" => Ok("text/markdown"),
        "json" => Ok("application/json"),
        _ => Err(Error::Invalid(
            "Feishu file is not a supported text attachment",
        )),
    }
}
