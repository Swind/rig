use std::collections::HashSet;

use rig_core::completion::message::{AssistantContent, Message, ToolResultContent, UserContent};

pub(crate) fn complete_exchange(messages: &[Message]) -> bool {
    let mut pending = HashSet::new();
    for message in messages {
        match message {
            Message::Assistant { content, .. } => {
                for item in content {
                    if let AssistantContent::ToolCall(call) = item
                        && !pending.insert(call.id.clone())
                    {
                        return false;
                    }
                }
            }
            Message::User { content } => {
                for item in content {
                    if let UserContent::ToolResult(result) = item
                        && !pending.remove(&result.call)
                    {
                        return false;
                    }
                }
            }
            Message::System { .. } => {}
        }
    }
    pending.is_empty()
}

fn append_text(output: &mut String, text: &str, budget: usize) {
    let mut length = text.len().min(budget.saturating_sub(output.len()));
    while !text.is_char_boundary(length) {
        length -= 1;
    }
    if let Some(text) = text.get(..length) {
        output.push_str(text);
    }
}

pub(crate) fn embedding_text(messages: &[Message], budget: usize) -> String {
    let mut output = String::new();
    for message in messages {
        match message {
            Message::System { content } => append_text(&mut output, content, budget),
            Message::User { content } => {
                for item in content {
                    match item {
                        UserContent::Text(text) => append_text(&mut output, &text.text, budget),
                        UserContent::ToolResult(result) => {
                            append_text(&mut output, result.name.as_str(), budget);
                            for value in &result.content {
                                match value {
                                    ToolResultContent::Text(text) => {
                                        append_text(&mut output, &text.text, budget);
                                    }
                                    ToolResultContent::Json { value } => {
                                        append_text(&mut output, &value.to_string(), budget);
                                    }
                                    ToolResultContent::Image(_) => {
                                        append_text(&mut output, "[image]", budget);
                                    }
                                }
                                append_text(&mut output, "\n", budget);
                            }
                        }
                        UserContent::Image(_) => append_text(&mut output, "[image]", budget),
                        UserContent::Audio(_) => append_text(&mut output, "[audio]", budget),
                        UserContent::Video(_) => append_text(&mut output, "[video]", budget),
                        UserContent::Document(_) => append_text(&mut output, "[document]", budget),
                    }
                    append_text(&mut output, "\n", budget);
                }
            }
            Message::Assistant { content, .. } => {
                for item in content {
                    match item {
                        AssistantContent::Text(text) => {
                            append_text(&mut output, &text.text, budget)
                        }
                        AssistantContent::ToolCall(call) => {
                            append_text(&mut output, call.function.name.as_str(), budget);
                            append_text(&mut output, " ", budget);
                            append_text(&mut output, &call.function.arguments.to_string(), budget);
                        }
                        AssistantContent::Image(_) => append_text(&mut output, "[image]", budget),
                        AssistantContent::Reasoning(_) => {}
                    }
                    append_text(&mut output, "\n", budget);
                }
            }
        }
        append_text(&mut output, "\n", budget);
        if output.len() == budget {
            break;
        }
    }
    output
}

#[cfg(test)]
mod tests;
