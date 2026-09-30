#![allow(clippy::panic_in_result_fn)]

use super::*;
use crate::test_support::{Call, FakeAdapter, inbound};
use rig_agent::run::PromptResponse;
use std::sync::atomic::Ordering;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn text(text: &str) -> Result<MultiTurnStreamItem, Box<dyn std::error::Error>> {
    let transcript = rig_core::streaming::Transcript::parse_prefix(serde_json::json!([
        {"item":"event","value":{"event":"start","part":0,"kind":"text"}},
        {"item":"event","value":{"event":"text","part":0,"text":text}}
    ]))?;
    let item = transcript
        .into_items()
        .pop()
        .ok_or_else(|| std::io::Error::other("missing text event"))?;
    Ok(MultiTurnStreamItem::StreamAssistantItem(item))
}
fn final_response(text: &str) -> MultiTurnStreamItem {
    MultiTurnStreamItem::FinalResponse(
        PromptResponse::new(text, Default::default())
            .with_memory_append(Some(MemoryAppend::Acknowledged)),
    )
}
fn stream(items: Vec<MultiTurnStreamItem>) -> StreamingResult {
    Box::pin(futures::stream::iter(items.into_iter().map(Ok)))
}
fn delayed(items: Vec<(u64, MultiTurnStreamItem)>) -> StreamingResult {
    Box::pin(futures::stream::iter(items).then(|(ms, item)| async move {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(item)
    }))
}

#[test]
fn rendering_converts_tables_splits_and_balances_fences() {
    let table = "before\n\n| name | value |\n| --- | --- |\n| hello | world |\n\nafter";
    let chunks = render_chunks(table, 60, TableMode::Code);
    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(chunk.chars().count() <= 60);
        assert_eq!(
            chunk.lines().filter(|line| line.starts_with("```")).count() % 2,
            0
        );
    }
    assert!(render_chunks(table, 2000, TableMode::Bullets)[0].contains("• name: hello"));
    assert_eq!(render_chunks(table, 2000, TableMode::Off), [table]);
    let code = format!("```rust\n{}\n```", "let x=1;\n".repeat(40));
    for chunk in render_chunks(&code, 80, TableMode::Off) {
        assert!(chunk.chars().count() <= 80);
        assert_eq!(
            chunk.lines().filter(|line| line.starts_with("```")).count() % 2,
            0
        );
    }
}

#[tokio::test(start_paused = true)]
async fn preview_is_throttled_and_final_text_is_authoritative() -> TestResult {
    let adapter = FakeAdapter::default();
    let input = delayed(vec![
        (0, text("a")?),
        (500, text("b")?),
        (1000, text("c")?),
        (10, text("d")?),
        (0, final_response("authoritative")),
    ]);
    egress(
        &adapter,
        &inbound("").reply_channel,
        input,
        &ChatConfig::default(),
    )
    .await?;
    let calls = adapter.calls();
    assert_eq!(calls.len(), 3);
    assert!(matches!(&calls[0],Call::Send(_,s) if s=="…"));
    assert!(matches!(&calls[1],Call::Edit(_,s) if s=="abc"));
    assert!(matches!(&calls[2],Call::Edit(_,s) if s=="authoritative"));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn retry_resets_preview_and_discarded_text() -> TestResult {
    let adapter = FakeAdapter::default();
    let input = delayed(vec![
        (0, text("rejected")?),
        (1600, text(" answer")?),
        (0, MultiTurnStreamItem::ModelTurnRetried { turn: 1 }),
        (1600, text("accepted")?),
        (0, final_response("accepted")),
    ]);
    egress(
        &adapter,
        &inbound("").reply_channel,
        input,
        &ChatConfig::default(),
    )
    .await?;
    let calls = adapter.calls();
    assert!(matches!(&calls[2],Call::Edit(_,s) if s=="…"));
    assert!(matches!(&calls[3],Call::Edit(_,s) if s=="accepted"));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn three_edit_failures_disable_previews_and_final_delivery_recovers() -> TestResult {
    let adapter = FakeAdapter::default();
    adapter.fail_edits.store(usize::MAX, Ordering::SeqCst);
    let input = delayed(vec![
        (0, text("a")?),
        (1600, text("b")?),
        (1600, text("c")?),
        (1600, text("d")?),
        (1600, text("e")?),
        (0, final_response("abcde")),
    ]);
    egress(
        &adapter,
        &inbound("").reply_channel,
        input,
        &ChatConfig::default(),
    )
    .await?;
    let calls = adapter.calls();
    assert_eq!(
        calls.iter().filter(|c| matches!(c, Call::Edit(..))).count(),
        4
    );
    assert!(matches!(&calls[calls.len() - 2], Call::Delete(_)));
    assert!(matches!(&calls[calls.len()-1],Call::Send(_,s) if s=="abcde"));
    Ok(())
}

#[tokio::test]
async fn failed_placeholder_creation_recovers_at_final_delivery() -> TestResult {
    let adapter = FakeAdapter::default();
    adapter.fail_sends.store(1, Ordering::SeqCst);
    egress(
        &adapter,
        &inbound("").reply_channel,
        stream(vec![text("preview")?, final_response("final")]),
        &ChatConfig::default(),
    )
    .await?;
    assert_eq!(adapter.calls().len(), 2);
    assert!(matches!(&adapter.calls()[1],Call::Send(_,s) if s=="final"));
    Ok(())
}

#[tokio::test]
async fn delete_failure_attempts_replacement_and_returns_error() -> TestResult {
    let adapter = FakeAdapter::default();
    adapter.fail_edits.store(1, Ordering::SeqCst);
    adapter.fail_deletes.store(true, Ordering::SeqCst);
    let result = egress(
        &adapter,
        &inbound("").reply_channel,
        stream(vec![text("preview")?, final_response("final")]),
        &ChatConfig::default(),
    )
    .await;
    assert!(result.is_err());
    assert!(matches!(&adapter.calls()[3],Call::Send(_,s) if s=="final"));
    Ok(())
}

#[tokio::test]
async fn uneditable_adapter_sends_chunks_once_and_preserves_native_tables() -> TestResult {
    let adapter = FakeAdapter {
        edits: false,
        native_tables: true,
        ..Default::default()
    };
    let table = "| a | b |\n| --- | --- |\n| c | d |";
    egress(
        &adapter,
        &inbound("").reply_channel,
        stream(vec![text("preview")?, final_response(table)]),
        &ChatConfig::default(),
    )
    .await?;
    assert_eq!(
        adapter.calls(),
        vec![Call::Send(inbound("").reply_channel, table.into())]
    );
    Ok(())
}

#[tokio::test]
async fn undelivered_chunk_returns_error_and_other_chunks_are_attempted() {
    let adapter = FakeAdapter {
        edits: false,
        limit: 10,
        ..Default::default()
    };
    adapter.fail_sends.store(1, Ordering::SeqCst);
    assert!(
        egress(
            &adapter,
            &inbound("").reply_channel,
            stream(vec![final_response("1234567890abcdefghij")]),
            &ChatConfig::default()
        )
        .await
        .is_err()
    );
    assert_eq!(adapter.calls().len(), 2);
}

#[tokio::test]
async fn empty_final_response_has_explanation() -> TestResult {
    let adapter = FakeAdapter {
        edits: false,
        ..Default::default()
    };
    egress(
        &adapter,
        &inbound("").reply_channel,
        stream(vec![final_response("")]),
        &ChatConfig::default(),
    )
    .await?;
    assert!(matches!(&adapter.calls()[0],Call::Send(_,s) if s.contains("without a text reply")));
    Ok(())
}

#[tokio::test]
async fn stream_error_and_unexpected_eof_replace_placeholder() -> TestResult {
    let adapter = FakeAdapter {
        limit: 30,
        ..Default::default()
    };
    let failure = rig_agent::agent::StreamingError::from(rig_core::memory::MemoryError::backend(
        std::io::Error::other("long error ".repeat(20)),
    ));
    let input: StreamingResult = Box::pin(futures::stream::iter(vec![
        Ok(text("preview")?),
        Err(failure),
    ]));
    assert!(matches!(
        egress(
            &adapter,
            &inbound("").reply_channel,
            input,
            &ChatConfig::default()
        )
        .await,
        Err(ChatError::Stream(_))
    ));
    assert!(matches!(&adapter.calls()[1],Call::Edit(_,s) if s.starts_with("⚠️")));
    for call in adapter.calls() {
        if let Call::Send(_, s) | Call::Edit(_, s) = call {
            assert!(s.chars().count() <= 30);
        }
    }
    let adapter = FakeAdapter::default();
    assert!(matches!(
        egress(
            &adapter,
            &inbound("").reply_channel,
            stream(vec![text("preview")?]),
            &ChatConfig::default()
        )
        .await,
        Err(ChatError::UnexpectedEnd)
    ));
    assert!(
        matches!(&adapter.calls()[1],Call::Edit(_,s) if s.contains("without a final response"))
    );
    Ok(())
}

#[tokio::test]
async fn missing_memory_acknowledgement_delivers_answer_and_warns() {
    let adapter = FakeAdapter {
        edits: false,
        ..Default::default()
    };
    let input = stream(vec![MultiTurnStreamItem::FinalResponse(
        PromptResponse::new("answer", Default::default()),
    )]);
    assert!(matches!(
        egress(
            &adapter,
            &inbound("").reply_channel,
            input,
            &ChatConfig::default()
        )
        .await,
        Err(ChatError::MissingMemory)
    ));
    assert!(
        matches!(&adapter.calls()[0],Call::Send(_,s) if s.starts_with("answer") && s.contains("persistence"))
    );
}
