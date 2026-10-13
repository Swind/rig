use crate::{
    completion::{CompletionRequest, ToolDefinition},
    message::{
        AssistantContent, AssistantMessage, AudioMediaType, DocumentMediaType, ImageMediaType,
        Message, ToolName, ToolResultContent, UserContent, VideoMediaType,
    },
    operation::Completion,
    providers::{anthropic, gemini, ollama, openai},
    test_utils::json_body,
    wire::{Encoded, Mode, Operation, Wire},
};
use serde_json::{Value, json};

fn body<W: Wire<Op = Completion, Payload = Encoded>>(
    wire: &W,
    request: CompletionRequest,
) -> Value {
    let request = Completion::prepare(request, &wire.describe()).expect("request prepares");
    json_body(
        &wire
            .encode(request, Mode::Unary)
            .expect("request encodes")
            .request,
    )
}

fn bodies(request: CompletionRequest) -> [Value; 4] {
    [
        body(
            &openai::OpenAIConfig::new("key").chat("gpt-4o-mini"),
            request.clone(),
        ),
        body(
            &openai::OpenAIConfig::new("key").responses("gpt-4o-mini"),
            request.clone(),
        ),
        body(
            &anthropic::AnthropicConfig::new("key").completion(anthropic::CLAUDE_SONNET_4_6),
            request.clone(),
        ),
        body(
            &gemini::GeminiConfig::new("key").completion("gemini-2.5-flash"),
            request,
        ),
    ]
}

/// Encoding equivalence is a local contract independent of a provider reply.
#[test]
fn raw_media_images_and_pdfs_match_base64_on_provider_wires() {
    for (raw, encoded) in [
        (
            UserContent::image_raw(vec![0, 255, 16], Some(ImageMediaType::PNG), None),
            UserContent::image_base64("AP8Q", Some(ImageMediaType::PNG), None),
        ),
        (
            UserContent::document_raw(vec![0, 255, 16], Some(DocumentMediaType::PDF)),
            UserContent::document_base64("AP8Q", Some(DocumentMediaType::PDF)),
        ),
    ] {
        assert_eq!(
            bodies(CompletionRequest::from(vec![Message::from(raw)])),
            bodies(CompletionRequest::from(vec![Message::from(encoded)])),
        );
    }
    let wire = ollama::OllamaConfig::new().native_completion("llava");
    let request = CompletionRequest::from(vec![Message::from(UserContent::image_raw(
        vec![0, 255, 16],
        Some(ImageMediaType::PNG),
        None,
    ))]);
    assert_eq!(
        body(&wire, request)["messages"][0]["images"],
        json!(["AP8Q"])
    );
}

/// A tool image must take the same byte encoding as a user image.
#[test]
fn raw_media_tool_images_match_base64_on_provider_wires() {
    let request = |image| {
        let name = ToolName::new("image_tool").expect("tool name");
        let mut request = CompletionRequest::from(vec![
            Message::user("show an image"),
            Message::Assistant(AssistantMessage::new(vec![AssistantContent::tool_call(
                "call_1",
                name.clone(),
                json!({}),
            )])),
            Message::from(UserContent::tool_result(
                crate::message::CallId::from_wire("call_1"),
                name.clone(),
                vec![ToolResultContent::text("image"), image],
            )),
        ]);
        request.tools = vec![ToolDefinition::new(
            name,
            "an image",
            json!({"type": "object", "properties": {}}),
        )];
        request
    };
    assert_eq!(
        bodies(request(ToolResultContent::image_raw(
            vec![0, 255, 16],
            Some(ImageMediaType::PNG),
            None,
        ))),
        bodies(request(ToolResultContent::image_base64(
            "AP8Q",
            Some(ImageMediaType::PNG),
            None,
        ))),
    );
}

/// Preparing inline audio and video must preserve their bytes.
#[test]
fn raw_media_audio_and_video_match_base64_on_supported_wires() {
    for (raw, encoded) in [
        (
            UserContent::audio_raw(vec![0, 255, 16], Some(AudioMediaType::MP3)),
            UserContent::audio_base64("AP8Q", Some(AudioMediaType::MP3)),
        ),
        (
            UserContent::video_raw(vec![0, 255, 16], Some(VideoMediaType::MP4)),
            UserContent::video_base64("AP8Q", Some(VideoMediaType::MP4)),
        ),
    ] {
        let raw = CompletionRequest::from(vec![Message::from(raw)]);
        let encoded = CompletionRequest::from(vec![Message::from(encoded)]);
        let gemini = gemini::GeminiConfig::new("key").completion("gemini-2.5-flash");
        assert_eq!(body(&gemini, raw.clone()), body(&gemini, encoded.clone()));
        let chat = openai::OpenAIConfig::new("key").chat("gpt-4o-mini");
        assert_eq!(body(&chat, raw), body(&chat, encoded));
    }
}

/// UTF-8 documents become text; unreadable bytes follow the adapter's placeholder policy.
#[test]
fn raw_media_text_documents_preserve_utf8() {
    let text = "附件內容";
    let raw = UserContent::document_raw(text.as_bytes().to_vec(), Some(DocumentMediaType::TXT));
    let string = UserContent::document_text(text, Some(DocumentMediaType::TXT));
    assert_eq!(
        bodies(CompletionRequest::from(vec![Message::from(raw.clone())])),
        bodies(CompletionRequest::from(vec![Message::from(string.clone())])),
    );
    let wire = ollama::OllamaConfig::new().native_completion("llava");
    assert_eq!(
        body(&wire, CompletionRequest::from(vec![Message::from(raw)])),
        body(&wire, CompletionRequest::from(vec![Message::from(string)])),
    );
    let invalid = Message::from(UserContent::document_raw(
        vec![255],
        Some(DocumentMediaType::TXT),
    ));
    assert_eq!(
        super::adapt(
            &[invalid],
            &openai::OpenAIConfig::new("key").responses("gpt-4o-mini")
        ),
        vec![Message::user(super::DOCUMENT_UNSENDABLE)],
    );
}
