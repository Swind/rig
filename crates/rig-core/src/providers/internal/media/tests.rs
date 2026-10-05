use crate::{
    message::{
        AudioMediaType, DocumentMediaType, ImageMediaType, Message, ToolName, ToolResultContent,
        UserContent, VideoMediaType,
    },
    providers::{anthropic, gemini, ollama, openai},
};
use serde_json::{Value, json};

fn encoded_messages(message: Message) -> [Value; 4] {
    [
        serde_json::to_value(
            Vec::<openai::completion::Message>::try_from(message.clone()).expect("chat conversion"),
        )
        .expect("chat serialization"),
        serde_json::to_value(
            Vec::<openai::responses_api::InputItem>::try_from(message.clone())
                .expect("responses conversion"),
        )
        .expect("responses serialization"),
        serde_json::to_value(
            anthropic::completion::Message::try_from(message.clone())
                .expect("anthropic conversion"),
        )
        .expect("anthropic serialization"),
        serde_json::to_value(
            gemini::completion::gemini_api_types::Content::try_from(message)
                .expect("gemini conversion"),
        )
        .expect("gemini serialization"),
    ]
}

/// Raw and base64 sources must produce identical requests against the same
/// recorded wire shapes. No service is needed to check encoding equivalence.
#[test]
fn raw_media_images_and_pdfs_match_base64_on_provider_wires() {
    let bytes = vec![0, 255, 16];
    for (raw, encoded) in [
        (
            UserContent::image_raw(bytes.clone(), Some(ImageMediaType::PNG), None),
            UserContent::image_base64("AP8Q", Some(ImageMediaType::PNG), None),
        ),
        (
            UserContent::document_raw(bytes.clone(), Some(DocumentMediaType::PDF)),
            UserContent::Document(crate::message::Document {
                data: crate::message::DocumentSourceKind::Base64("AP8Q".into()),
                media_type: Some(DocumentMediaType::PDF),
                additional_params: None,
            }),
        ),
    ] {
        assert_eq!(
            encoded_messages(raw.into()),
            encoded_messages(encoded.into())
        );
    }
    let image = Message::from(UserContent::image_raw(
        bytes,
        Some(ImageMediaType::PNG),
        None,
    ));
    let ollama =
        serde_json::to_value(Vec::<ollama::Message>::try_from(image).expect("ollama conversion"))
            .expect("ollama serialization");
    assert_eq!(ollama[0]["images"], json!(["AP8Q"]));
}

/// Tool images follow the same byte encoding as user images. Cassette
/// image-input cells separately exercise the requests against recorded traffic.
#[test]
fn raw_media_tool_images_match_base64_on_provider_wires() {
    let tool_message = |image| {
        Message::from(UserContent::tool_result(
            crate::message::CallId::from_wire("call_1"),
            ToolName::new("image_tool").expect("tool name"),
            vec![ToolResultContent::text("image"), image],
        ))
    };
    assert_eq!(
        encoded_messages(tool_message(ToolResultContent::image_raw(
            vec![0, 255, 16],
            Some(ImageMediaType::PNG),
            None,
        ))),
        encoded_messages(tool_message(ToolResultContent::image_base64(
            "AP8Q",
            Some(ImageMediaType::PNG),
            None,
        ))),
    );
}

/// Audio and video codecs must preserve the bytes; model capability remains
/// the provider's responsibility rather than this encoding check.
#[test]
fn raw_media_audio_and_video_match_base64_on_supported_wires() {
    for (raw, encoded) in [
        (
            UserContent::audio_raw(vec![0, 255, 16], Some(AudioMediaType::MP3)),
            UserContent::audio("AP8Q", Some(AudioMediaType::MP3)),
        ),
        (
            UserContent::video_raw(vec![0, 255, 16], Some(VideoMediaType::MP4)),
            UserContent::video("AP8Q", Some(VideoMediaType::MP4)),
        ),
    ] {
        assert_eq!(
            serde_json::to_value(
                openai::completion::UserContent::try_from(raw.clone()).expect("raw chat media")
            )
            .expect("serialize"),
            serde_json::to_value(
                openai::completion::UserContent::try_from(encoded.clone())
                    .expect("base64 chat media")
            )
            .expect("serialize"),
        );
        assert_eq!(
            serde_json::to_value(
                gemini::completion::gemini_api_types::Part::try_from(raw)
                    .expect("raw gemini media")
            )
            .expect("serialize"),
            serde_json::to_value(
                gemini::completion::gemini_api_types::Part::try_from(encoded)
                    .expect("base64 gemini media")
            )
            .expect("serialize"),
        );
    }
}

/// Text bytes must reach providers as text, including Unicode, rather than
/// their base64 representation. This is a local UTF-8 conversion contract.
#[test]
fn raw_media_text_documents_preserve_utf8_and_reject_invalid_bytes() {
    let text = "附件內容";
    let raw = UserContent::document_raw(text.as_bytes().to_vec(), Some(DocumentMediaType::TXT));
    let string = UserContent::document(text, Some(DocumentMediaType::TXT));
    assert_eq!(
        encoded_messages(raw.clone().into()),
        encoded_messages(string.clone().into())
    );
    assert_eq!(
        serde_json::to_value(
            Vec::<ollama::Message>::try_from(Message::from(raw)).expect("raw text")
        )
        .expect("serialize"),
        serde_json::to_value(
            Vec::<ollama::Message>::try_from(Message::from(string)).expect("string text")
        )
        .expect("serialize"),
    );
    let invalid = UserContent::document_raw(vec![255], Some(DocumentMediaType::TXT));
    assert!(openai::completion::UserContent::try_from(invalid.clone()).is_err());
    assert!(
        Vec::<openai::responses_api::InputItem>::try_from(Message::from(invalid.clone())).is_err()
    );
    assert!(anthropic::completion::Message::try_from(Message::from(invalid.clone())).is_err());
    assert!(gemini::completion::gemini_api_types::Part::try_from(invalid.clone()).is_err());
    assert!(Vec::<ollama::Message>::try_from(Message::from(invalid)).is_err());
}
