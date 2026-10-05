use std::io;

use serde_json::json;

use super::*;

#[test]
fn request_json_defaults_and_required_query() -> anyhow::Result<()> {
    let request: ConversationSearchRequest = serde_json::from_value(json!({"query": "Cypher"}))?;
    anyhow::ensure!(request.limit == 5 && request.conversation_id.is_none());
    anyhow::ensure!(serde_json::from_value::<ConversationSearchRequest>(json!({})).is_err());

    let scoped: ConversationSearchRequest = serde_json::from_value(json!({
        "query": "  Cypher 東京  ",
        "limit": 1,
        "conversation_id": "conversation-1",
    }))?;
    scoped.validate()?;
    anyhow::ensure!(scoped.query == "  Cypher 東京  ");
    anyhow::ensure!(scoped.conversation_id == Some("conversation-1".into()));
    anyhow::ensure!(serde_json::to_value(scoped)?["conversation_id"] == "conversation-1");
    Ok(())
}

#[test]
fn validation_rejects_blank_queries_and_out_of_range_limits() -> anyhow::Result<()> {
    for query in ["", " \t\n", "\u{2003}"] {
        let request = ConversationSearchRequest {
            query: query.into(),
            limit: 5,
            conversation_id: None,
        };
        anyhow::ensure!(matches!(
            request.validate(),
            Err(ConversationSearchError::InvalidRequest { .. })
        ));
    }
    for limit in [0, 101, u32::MAX] {
        let request = ConversationSearchRequest {
            query: "Cypher".into(),
            limit,
            conversation_id: None,
        };
        anyhow::ensure!(matches!(
            request.validate(),
            Err(ConversationSearchError::InvalidRequest { .. })
        ));
    }
    for limit in [1, 100] {
        ConversationSearchRequest {
            query: "Cypher".into(),
            limit,
            conversation_id: None,
        }
        .validate()?;
    }
    Ok(())
}

fn excerpt() -> ConversationSearchHit {
    ConversationSearchHit {
        conversation_id: "conversation-1".into(),
        chunk_id: "chunk-2".into(),
        start_index: 12,
        messages: vec![
            Message::user("How does Cypher work?"),
            Message::assistant("It queries graph data."),
        ],
    }
}

#[test]
fn hit_json_preserves_source_reference_and_original_messages() -> anyhow::Result<()> {
    let hit = excerpt();
    let serialized = serde_json::to_value(&hit)?;
    anyhow::ensure!(serialized["conversation_id"] == "conversation-1");
    anyhow::ensure!(serialized["chunk_id"] == "chunk-2");
    anyhow::ensure!(serialized["start_index"] == 12);
    anyhow::ensure!(serde_json::from_value::<ConversationSearchHit>(serialized)? == hit);
    Ok(())
}

struct MockSearch;

impl ConversationSearch for MockSearch {
    async fn search(
        &self,
        request: ConversationSearchRequest,
    ) -> Result<Vec<ConversationSearchHit>, ConversationSearchError> {
        request.validate()?;
        if request.conversation_id != Some("conversation-1".into()) {
            return Ok(vec![]);
        }
        Ok(vec![excerpt()])
    }
}

#[tokio::test]
async fn generic_consumer_reads_original_excerpts_and_empty_matches() -> anyhow::Result<()> {
    async fn search(
        backend: &impl ConversationSearch,
        conversation_id: Option<ConversationId>,
    ) -> Result<Vec<ConversationSearchHit>, ConversationSearchError> {
        backend
            .search(ConversationSearchRequest {
                query: "Cypher".into(),
                limit: 1,
                conversation_id,
            })
            .await
    }

    anyhow::ensure!(search(&MockSearch, Some("conversation-1".into())).await? == vec![excerpt()]);
    anyhow::ensure!(search(&MockSearch, Some("other".into())).await?.is_empty());
    Ok(())
}

#[test]
fn backend_error_preserves_original_source() -> anyhow::Result<()> {
    let error = ConversationSearchError::backend(io::Error::other("index unavailable"));
    anyhow::ensure!(matches!(&error, ConversationSearchError::Backend { .. }));
    let source = error
        .source()
        .ok_or_else(|| anyhow::anyhow!("backend error lost its source"))?;
    anyhow::ensure!(source.downcast_ref::<io::Error>().is_some());
    anyhow::ensure!(source.to_string() == "index unavailable");

    let invalid = ConversationSearchRequest {
        query: " ".into(),
        limit: 5,
        conversation_id: None,
    }
    .validate();
    let Err(invalid) = invalid else {
        anyhow::bail!("blank query passed validation");
    };
    anyhow::ensure!(invalid.source().is_none());
    anyhow::ensure!(invalid.to_string().contains("non-whitespace"));
    Ok(())
}
