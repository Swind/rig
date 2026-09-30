use crate::{
    ChannelRef, ChatAdapter, ChatConfig, ChatError, MessageRef, format,
    markdown::{self, TableMode},
};
use futures::StreamExt;
use rig_agent::{
    agent::{MultiTurnStreamItem, StreamingResult},
    run::MemoryAppend,
};
use rig_core::streaming::{Item, StreamEvent};
use tokio::time::{Duration, Instant};

fn render_chunks(text: &str, limit: usize, table_mode: TableMode) -> Vec<String> {
    format::split_message(&markdown::convert_tables(text, table_mode), limit)
}

async fn deliver(
    adapter: &dyn ChatAdapter,
    channel: &ChannelRef,
    placeholder: Option<&MessageRef>,
    chunks: Vec<String>,
) -> Result<(), ChatError> {
    let mut failure = None;
    for (index, chunk) in chunks.iter().enumerate() {
        if index == 0
            && let Some(message) = placeholder
        {
            if adapter.edit(message, chunk).await.is_ok() {
                continue;
            }
            if let Err(error) = adapter.delete(message).await {
                tracing::debug!(?message, %error, "could not delete provisional message");
                failure = Some(error);
            }
        }
        if let Err(error) = adapter.send(channel, chunk).await {
            tracing::debug!(%error, "final message delivery failed");
            if failure.is_none() {
                failure = Some(error);
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

pub(crate) async fn egress(
    adapter: &dyn ChatAdapter,
    channel: &ChannelRef,
    mut stream: StreamingResult,
    cfg: &ChatConfig,
) -> Result<(), ChatError> {
    let limit = adapter.message_limit();
    let mut preview_enabled = adapter.supports_edit();
    let mut buf = String::new();
    let mut placeholder = None;
    let mut edit_failures = 0;
    let mut next_edit = Instant::now();
    let interval = Duration::from_millis(1500);
    let (mut text, terminal_error) = loop {
        match stream.next().await {
            Some(Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(
                StreamEvent::Text { text, .. },
            )))) => {
                buf.push_str(&text);
                if !preview_enabled {
                    continue;
                }
                if placeholder.is_none() {
                    match adapter.send(channel, "…").await {
                        Ok(message) => {
                            placeholder = Some(message);
                            next_edit = Instant::now() + interval;
                        }
                        Err(error) => {
                            tracing::debug!(%error, "preview creation failed");
                            preview_enabled = false;
                        }
                    }
                } else if Instant::now() >= next_edit
                    && let Some(message) = placeholder.as_ref()
                {
                    let preview =
                        format::truncate_chars_tail(&buf, limit.saturating_sub(100).max(1));
                    match adapter.edit(message, &preview).await {
                        Ok(()) => edit_failures = 0,
                        Err(error) => {
                            tracing::debug!(%error, "preview edit failed");
                            edit_failures += 1;
                        }
                    }
                    next_edit = Instant::now() + interval;
                    if edit_failures >= 3 {
                        preview_enabled = false;
                    }
                }
            }
            Some(Ok(MultiTurnStreamItem::ModelTurnRetried { .. })) => {
                buf.clear();
                if preview_enabled && let Some(message) = placeholder.as_ref() {
                    if let Err(error) = adapter.edit(message, "…").await {
                        tracing::debug!(%error, "retry preview reset failed");
                        edit_failures += 1;
                        if edit_failures >= 3 {
                            preview_enabled = false;
                        }
                    } else {
                        edit_failures = 0;
                    }
                    next_edit = Instant::now() + interval;
                }
            }
            Some(Ok(MultiTurnStreamItem::FinalResponse(response))) => {
                let error = match response.memory_append() {
                    Some(MemoryAppend::Acknowledged) => None,
                    Some(MemoryAppend::Failed { report }) => {
                        Some(ChatError::MemoryAppend(report.clone()))
                    }
                    None => Some(ChatError::MissingMemory),
                };
                break (response.output().to_string(), error);
            }
            Some(Err(error)) => break (format!("⚠️ {error}"), Some(ChatError::Stream(error))),
            None => {
                break (
                    "⚠️ Agent stream ended without a final response.".to_string(),
                    Some(ChatError::UnexpectedEnd),
                );
            }
            Some(Ok(_)) => {}
        }
    };
    if text.trim().is_empty() {
        text = "The agent completed without a text reply.".to_string();
    }
    if matches!(
        terminal_error,
        Some(ChatError::MemoryAppend(_) | ChatError::MissingMemory)
    ) {
        text.push_str(
            "\n⚠️ History persistence was not acknowledged. The next turn may lack this reply.",
        );
    }
    let mode = if adapter.renders_native_tables() {
        TableMode::Off
    } else {
        cfg.table_mode
    };
    let delivery = deliver(
        adapter,
        channel,
        placeholder.as_ref(),
        render_chunks(&text, limit, mode),
    )
    .await;
    if let Some(error) = terminal_error {
        if let Err(delivery_error) = delivery {
            tracing::debug!(%delivery_error, "error notification delivery failed");
        }
        Err(error)
    } else {
        delivery
    }
}

#[cfg(test)]
mod tests;
