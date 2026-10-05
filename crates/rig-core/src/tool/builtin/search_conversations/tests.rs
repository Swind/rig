use std::{error::Error, io, sync::Arc, sync::Mutex};

use super::*;
use crate::{completion::Message, tool::IntoToolOutput, tool::tool_definition};

#[derive(Clone, Default)]
struct MockSearch {
    requests: Arc<Mutex<Vec<ConversationSearchRequest>>>,
    hits: Vec<ConversationSearchHit>,
    fail: bool,
}

impl ConversationSearch for MockSearch {
    async fn search(
        &self,
        request: ConversationSearchRequest,
    ) -> Result<Vec<ConversationSearchHit>, ConversationSearchError> {
        self.requests
            .lock()
            .map_err(|_| ConversationSearchError::backend(io::Error::other("mock lock poisoned")))?
            .push(request);
        if self.fail {
            return Err(ConversationSearchError::backend(io::Error::other(
                "index unavailable",
            )));
        }
        Ok(self.hits.clone())
    }
}

fn request(query: &str, limit: u32) -> ConversationSearchRequest {
    ConversationSearchRequest {
        query: query.into(),
        limit,
        conversation_id: None,
    }
}

fn hit(chunk_id: &str, start_index: u64) -> ConversationSearchHit {
    ConversationSearchHit {
        conversation_id: "conversation-1".into(),
        chunk_id: chunk_id.into(),
        start_index,
        messages: vec![
            Message::user("How do I search conversations?"),
            Message::assistant("Use the search tool."),
        ],
    }
}

#[test]
fn schema_matches_argument_defaults_and_bounds() -> anyhow::Result<()> {
    let tool = SearchConversationsTool::new(MockSearch::default());
    let definition = tool_definition(&tool);
    anyhow::ensure!(definition.name == "search_conversations");
    let schema = tool.parameters();
    anyhow::ensure!(schema["required"] == json!(["query"]));
    anyhow::ensure!(schema["properties"]["limit"]["minimum"] == 1);
    anyhow::ensure!(schema["properties"]["limit"]["maximum"] == 100);
    anyhow::ensure!(schema["properties"]["conversation_id"]["type"] == json!(["string", "null"]));
    let args: ConversationSearchRequest = serde_json::from_value(json!({"query": "history"}))?;
    anyhow::ensure!(schema["properties"]["limit"]["default"] == args.limit);
    anyhow::ensure!(args.limit == 5 && args.conversation_id.is_none());
    let args: ConversationSearchRequest =
        serde_json::from_value(json!({"query": "history", "conversation_id": null}))?;
    anyhow::ensure!(args.conversation_id.is_none());
    anyhow::ensure!(serde_json::from_value::<ConversationSearchRequest>(json!({})).is_err());
    Ok(())
}

#[tokio::test]
async fn forwards_unchanged_query_limit_and_optional_scope() -> anyhow::Result<()> {
    let backend = MockSearch::default();
    let requests = backend.requests.clone();
    let tool = SearchConversationsTool::new(backend);
    let mut scoped = request("  O'Reilly 的 Cypher\n", 100);
    scoped.conversation_id = Some("conversation-1".into());
    let unscoped = request("history", 1);
    anyhow::ensure!(tool.call(scoped.clone()).await?.is_empty());
    anyhow::ensure!(tool.call(unscoped.clone()).await?.is_empty());
    let actual = requests
        .lock()
        .map_err(|_| io::Error::other("mock lock poisoned"))?;
    anyhow::ensure!(*actual == vec![scoped, unscoped]);
    Ok(())
}

#[tokio::test]
async fn rejects_invalid_requests_before_backend_invocation() -> anyhow::Result<()> {
    let backend = MockSearch::default();
    let requests = backend.requests.clone();
    let tool = SearchConversationsTool::new(backend);
    for args in [
        request("", 5),
        request(" \n\t", 5),
        request("history", 0),
        request("history", 101),
        request("history", u32::MAX),
    ] {
        anyhow::ensure!(matches!(
            tool.call(args).await,
            Err(ConversationSearchError::InvalidRequest { .. })
        ));
    }
    let actual = requests
        .lock()
        .map_err(|_| io::Error::other("mock lock poisoned"))?;
    anyhow::ensure!(actual.is_empty());
    Ok(())
}

#[tokio::test]
async fn bounds_result_count_without_reordering_or_changing_messages() -> anyhow::Result<()> {
    let expected = vec![hit("relevant-later-excerpt", 20), hit("earlier-excerpt", 2)];
    let mut hits = expected.clone();
    hits.push(hit("excess-excerpt", 10));
    let tool = SearchConversationsTool::new(MockSearch {
        hits,
        ..Default::default()
    });
    let actual = tool.call(request("history", 2)).await?;
    anyhow::ensure!(actual == expected);
    let output = actual.into_tool_output()?;
    let expected_json = serde_json::to_value(expected)?;
    anyhow::ensure!(output.as_json() == Some(&expected_json));
    Ok(())
}

#[tokio::test]
async fn empty_results_remain_a_structured_empty_array() -> anyhow::Result<()> {
    let tool = SearchConversationsTool::new(MockSearch::default());
    let output = tool.call(request("history", 5)).await?.into_tool_output()?;
    anyhow::ensure!(output.as_json() == Some(&json!([])));
    Ok(())
}

#[tokio::test]
async fn preserves_backend_error_source() -> anyhow::Result<()> {
    let tool = SearchConversationsTool::new(MockSearch {
        fail: true,
        ..Default::default()
    });
    let Err(error) = tool.call(request("history", 5)).await else {
        anyhow::bail!("backend failure must be returned by the tool");
    };
    anyhow::ensure!(matches!(error, ConversationSearchError::Backend { .. }));
    let source = error
        .source()
        .ok_or_else(|| io::Error::other("missing backend source"))?;
    anyhow::ensure!(source.downcast_ref::<io::Error>().is_some());
    anyhow::ensure!(source.to_string() == "index unavailable");
    Ok(())
}
