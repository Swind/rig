use anyhow::ensure;
use rig_core::completion::message::{
    AssistantContent, DocumentSourceKind, Image, Message, Text, ToolCall, ToolFunction, ToolName,
    ToolResultContent, UserContent,
};

use super::{Storage, StorageError};

#[tokio::test]
async fn stale_upsert_acknowledgments_do_not_complete_clear_cleanup() -> anyhow::Result<()> {
    let store = Storage::open(":memory:", "test".into()).await?;
    let id = "conversation".into();
    store
        .append("scope", &id, vec![Message::user("old")], 100)
        .await?;
    let work = store.pending("scope", 1).await?;
    let chunk_id = item(&work, 0)?.id.clone();
    store.clear("scope", &id).await?;
    store.mark_projection(&chunk_id, false).await?;
    let work = store.pending("scope", 1).await?;
    let cleanup = item(&work, 0)?;
    ensure!(cleanup.retired && !cleanup.vector_done);
    store.mark_projection(&chunk_id, true).await?;
    ensure!(store.pending("scope", 1).await?.is_empty());
    Ok(())
}

fn item<T>(items: &[T], index: usize) -> anyhow::Result<&T> {
    items
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("missing item at index {index}"))
}

#[tokio::test]
async fn adjacency_uses_original_order_and_isolates_scope_conversation_and_generation()
-> anyhow::Result<()> {
    let store = Storage::open(":memory:", "test".into()).await?;
    let id = "conversation".into();
    for text in ["first", "middle", "last"] {
        store
            .append("scope", &id, vec![Message::user(text)], 100)
            .await?;
    }
    store
        .append("other", &id, vec![Message::user("private")], 100)
        .await?;
    store
        .append(
            "scope",
            &"different".into(),
            vec![Message::user("unrelated")],
            100,
        )
        .await?;
    let chunks: Vec<_> = store
        .pending("scope", 10)
        .await?
        .into_iter()
        .filter(|chunk| chunk.conversation_id == id)
        .collect();
    let first = item(&chunks, 0)?;
    let middle = item(&chunks, 1)?;
    let last = item(&chunks, 2)?;
    store.mark_projection(&last.id, false).await?;
    store.mark_projection(&first.id, false).await?;
    ensure!(store.neighbors(middle, 10).await? == vec![first.id.clone(), last.id.clone()]);
    ensure!(store.neighbors(middle, 1).await? == vec![first.id.clone()]);
    ensure!(store.neighbors(middle, 0).await?.is_empty());
    ensure!(store.neighbors(first, 10).await? == vec![middle.id.clone()]);
    let mut mismatched = middle.clone();
    mismatched.scope = "other".into();
    ensure!(store.neighbors(&mismatched, 10).await?.is_empty());
    mismatched = middle.clone();
    mismatched.generation += 1;
    ensure!(store.neighbors(&mismatched, 10).await?.is_empty());
    store.clear("scope", &id).await?;
    for text in ["new first", "new last"] {
        store
            .append("scope", &id, vec![Message::user(text)], 100)
            .await?;
    }
    ensure!(store.neighbors(middle, 10).await?.is_empty());
    let active: Vec<_> = store
        .pending("scope", 10)
        .await?
        .into_iter()
        .filter(|chunk| !chunk.retired && chunk.conversation_id == id)
        .collect();
    let new_first = item(&active, 0)?;
    let new_last = item(&active, 1)?;
    ensure!(store.neighbors(new_first, 10).await? == vec![new_last.id.clone()]);
    Ok(())
}

#[tokio::test]
async fn originals_chunks_and_config_survive_reopen() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("history.sqlite");
    let id = "conversation".into();
    let messages = vec![
        Message::User {
            content: vec![
                UserContent::Text(Text::new("東京")),
                UserContent::Image(Image {
                    data: DocumentSourceKind::Raw(vec![0, 1, 255]),
                    ..Image::default()
                }),
            ],
        },
        Message::assistant("hello"),
    ];
    let store = Storage::open(&path, "model:3:chunk1".into()).await?;
    store.append("owner", &id, messages.clone(), 200).await?;
    let chunks = store.pending("owner", 10).await?;
    ensure!(chunks.len() == 1);
    let stable_id = item(&chunks, 0)?.id.clone();
    ensure!(item(&chunks, 0)?.start == 0 && item(&chunks, 0)?.end == 2);
    store.record_failure(&stable_id, "vector offline").await?;
    drop(store);
    let reopened = Storage::open(&path, "model:3:chunk1".into()).await?;
    ensure!(reopened.load("owner", &id).await? == messages);
    let work = reopened.pending("owner", 10).await?;
    ensure!(work.len() == 1 && item(&work, 0)?.id == stable_id);
    ensure!(!item(&work, 0)?.vector_done);
    let status = reopened.status("owner").await?;
    ensure!(status.pending_vector == 1);
    ensure!(status.failed_attempts == 1);
    ensure!(status.last_error.as_deref() == Some("vector offline"));
    let mismatched = Storage::open(&path, "model:4:chunk1".into()).await;
    ensure!(matches!(
        mismatched,
        Err(StorageError::IncompatibleConfiguration)
    ));
    reopened.mark_projection(&stable_id, false).await?;
    ensure!(reopened.pending("owner", 10).await?.is_empty());
    ensure!(reopened.active("owner", &[stable_id]).await?.len() == 1);
    Ok(())
}

#[tokio::test]
async fn scopes_clear_and_hydration_remain_isolated() -> anyhow::Result<()> {
    let store = Storage::open(":memory:", "test".into()).await?;
    let id = "shared-id".into();
    store.append("a", &id, Vec::new(), 100).await?;
    ensure!(store.pending("a", 10).await?.is_empty());
    store
        .append("a", &id, vec![Message::user("old")], 100)
        .await?;
    store
        .append("b", &id, vec![Message::user("private")], 100)
        .await?;
    let old_id = item(&store.pending("a", 10).await?, 0)?.id.clone();
    ensure!(
        store
            .hydrate("b", std::slice::from_ref(&old_id), None, 1000)
            .await?
            .is_empty()
    );
    store.clear("a", &id).await?;
    ensure!(store.load("a", &id).await?.is_empty());
    ensure!(store.load("b", &id).await? == vec![Message::user("private")]);
    ensure!(
        store
            .active("a", std::slice::from_ref(&old_id))
            .await?
            .is_empty()
    );
    ensure!(
        store
            .hydrate("a", std::slice::from_ref(&old_id), None, 1000)
            .await?
            .is_empty()
    );
    store
        .append("a", &id, vec![Message::user("new")], 100)
        .await?;
    let work = store.pending("a", 10).await?;
    ensure!(work.len() == 2);
    ensure!(item(&work, 0)?.retired && item(&work, 0)?.text.is_empty());
    ensure!(
        !item(&work, 1)?.retired && item(&work, 1)?.generation == 1 && item(&work, 1)?.start == 0
    );
    let new_id = item(&work, 1)?.id.clone();
    ensure!(new_id != old_id);
    let hits = store
        .hydrate(
            "a",
            &[old_id.clone(), new_id.clone(), new_id.clone()],
            None,
            1000,
        )
        .await?;
    ensure!(hits.len() == 1 && item(&hits, 0)?.messages == vec![Message::user("new")]);
    ensure!(
        store
            .hydrate("a", std::slice::from_ref(&new_id), None, 1)
            .await?
            .is_empty()
    );
    ensure!(
        store
            .hydrate("a", &[new_id], Some(&"different".into()), 1000)
            .await?
            .is_empty()
    );
    store.mark_projection(&old_id, true).await?;

    ensure!(store.pending("a", 10).await?.len() == 1);
    Ok(())
}

#[tokio::test]
async fn concurrent_handles_allocate_contiguous_positions() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("parallel.sqlite");
    let first = Storage::open(&path, "test".into()).await?;
    let second = Storage::open(&path, "test".into()).await?;
    let id = "conversation".into();
    let (left, right) = tokio::join!(
        first.append(
            "scope",
            &id,
            vec![Message::user("a"), Message::assistant("b")],
            100
        ),
        second.append(
            "scope",
            &id,
            vec![Message::user("c"), Message::assistant("d")],
            100
        ),
    );
    left?;
    right?;
    let loaded = first.load("scope", &id).await?;
    ensure!(loaded.len() == 4);
    ensure!(
        loaded
            == vec![
                Message::user("a"),
                Message::assistant("b"),
                Message::user("c"),
                Message::assistant("d")
            ]
            || loaded
                == vec![
                    Message::user("c"),
                    Message::assistant("d"),
                    Message::user("a"),
                    Message::assistant("b")
                ]
    );
    let chunks = first.pending("scope", 10).await?;
    ensure!(chunks.len() == 2);
    ensure!(item(&chunks, 0)?.start == 0 && item(&chunks, 0)?.end == 2);
    ensure!(item(&chunks, 1)?.start == 2 && item(&chunks, 1)?.end == 4);
    Ok(())
}

#[tokio::test]
async fn partial_tool_exchanges_wait_and_originals_roundtrip() -> anyhow::Result<()> {
    let store = Storage::open(":memory:", "test".into()).await?;
    let id = "conversation".into();
    let call = ToolCall::from_dual_wire(
        "fc-item",
        "call-correlator",
        ToolFunction::new(ToolName::new("lookup")?, serde_json::json!({"name":"東京"})),
    )
    .with_signature(Some("opaque signature".into()));
    let assistant = Message::Assistant {
        id: Some("provider-message".into()),
        content: vec![AssistantContent::ToolCall(call.clone())],
    };
    let result = Message::User {
        content: vec![UserContent::ToolResult(call.result(vec![
            ToolResultContent::Text(Text::new("literal")),
            ToolResultContent::Json {
                value: serde_json::json!({"complete":true}),
            },
        ]))],
    };
    store
        .append(
            "scope",
            &id,
            vec![Message::user("lookup"), assistant.clone()],
            100,
        )
        .await?;
    ensure!(store.pending("scope", 10).await?.is_empty());
    ensure!(store.load("scope", &id).await?.len() == 2);
    store
        .append(
            "scope",
            &id,
            vec![result.clone(), Message::assistant("done")],
            100,
        )
        .await?;
    let work = store.pending("scope", 10).await?;
    ensure!(work.len() == 1 && item(&work, 0)?.start == 0 && item(&work, 0)?.end == 4);
    let hits = store
        .hydrate("scope", &[item(&work, 0)?.id.clone()], None, 10000)
        .await?;
    ensure!(
        item(&hits, 0)?.messages
            == vec![
                Message::user("lookup"),
                assistant,
                result,
                Message::assistant("done")
            ]
    );
    ensure!(
        store
            .hydrate("scope", &[item(&work, 0)?.id.clone()], None, 100)
            .await?
            .is_empty()
    );
    store
        .append("scope", &id, vec![Message::user("same")], 100)
        .await?;
    store
        .append("scope", &id, vec![Message::user("same")], 100)
        .await?;
    ensure!(store.load("scope", &id).await?.len() == 6);
    ensure!(store.pending("scope", 10).await?.len() == 3);
    Ok(())
}

#[tokio::test]
async fn hydration_budget_includes_source_metadata_and_array_encoding() -> anyhow::Result<()> {
    let store = Storage::open(":memory:", "test".into()).await?;
    let id = rig_core::id::ConversationId::new("long-source-id".repeat(50));
    store
        .append("scope", &id, vec![Message::user("tiny")], 100)
        .await?;
    let chunks = store.pending("scope", 1).await?;
    let chunk_id = item(&chunks, 0)?.id.clone();
    ensure!(
        store
            .hydrate("scope", std::slice::from_ref(&chunk_id), None, 100)
            .await?
            .is_empty()
    );
    let hits = store
        .hydrate("scope", std::slice::from_ref(&chunk_id), None, 10000)
        .await?;
    let bytes = serde_json::to_vec(&hits)?.len();
    ensure!(
        store
            .hydrate("scope", std::slice::from_ref(&chunk_id), None, bytes)
            .await?
            == hits
    );
    ensure!(
        store
            .hydrate("scope", &[chunk_id], None, bytes - 1)
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn rebuild_reuses_original_ids_without_resetting_cleanup_or_other_scopes()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("rebuild.sqlite");
    let store = Storage::open(&path, "test".into()).await?;
    let id = "conversation".into();
    let deleted = "deleted".into();
    let messages = vec![Message::user("original"), Message::assistant("answer")];
    store.append("a", &id, messages.clone(), 100).await?;
    store.append("b", &id, messages.clone(), 100).await?;
    let active = store.pending("a", 10).await?;
    let stable_id = item(&active, 0)?.id.clone();
    for chunk in active.into_iter().chain(store.pending("b", 10).await?) {
        store.mark_projection(&chunk.id, false).await?;
    }
    store
        .append("a", &deleted, vec![Message::user("retired")], 100)
        .await?;
    store.clear("a", &deleted).await?;
    let retired = store.pending("a", 10).await?;
    let retired_id = item(&retired, 0)?.id.clone();
    store.rebuild("a").await?;
    drop(store);
    let reopened = Storage::open(&path, "test".into()).await?;
    let work = reopened.pending("a", 10).await?;
    ensure!(work.len() == 2);
    let cleanup = item(&work, 0)?;
    ensure!(cleanup.id == retired_id && cleanup.retired && !cleanup.vector_done);
    let active = item(&work, 1)?;
    ensure!(active.id == stable_id && !active.retired && !active.vector_done);
    ensure!(reopened.pending("b", 10).await?.is_empty());
    ensure!(reopened.load("a", &id).await? == messages);
    Ok(())
}
