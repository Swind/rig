use anyhow::ensure;
use rig_core::completion::message::{
    AssistantContent, Message, ToolCall, ToolFunction, ToolName, UserContent,
};

use super::{complete_exchange, embedding_text};

#[test]
fn text_budget_preserves_unicode_boundaries() {
    let messages = vec![Message::user("東京abcd")];
    assert_eq!(embedding_text(&messages, 5), "東\n\n");
    assert_eq!(embedding_text(&messages, 0), "");
}

#[test]
fn tool_pair_identity_and_duplicate_results_are_checked() -> anyhow::Result<()> {
    let call = ToolCall::from_wire(
        "call",
        ToolFunction::new(ToolName::new("tool")?, serde_json::json!({})),
    );
    let assistant = Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(call.clone())],
    };
    let result = Message::User {
        content: vec![UserContent::ToolResult(call.result(Vec::new()))],
    };
    ensure!(!complete_exchange(std::slice::from_ref(&assistant)));
    ensure!(!complete_exchange(std::slice::from_ref(&result)));
    ensure!(complete_exchange(&[assistant.clone(), result.clone()]));
    ensure!(!complete_exchange(&[assistant, result.clone(), result]));
    Ok(())
}
