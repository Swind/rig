//! Offline facade coverage for conversation search through contextual tool dispatch.

#![cfg(feature = "agent")]

use rig::{
    completion::Message,
    conversation_search::{
        ConversationSearch, ConversationSearchError, ConversationSearchHit,
        ConversationSearchRequest,
    },
    tool::{ToolContext, ToolSet, builtin::SearchConversationsTool},
};
use serde_json::json;

struct ScopedSearch;

impl ConversationSearch for ScopedSearch {
    async fn search(
        &self,
        request: ConversationSearchRequest,
    ) -> Result<Vec<ConversationSearchHit>, ConversationSearchError> {
        request.validate()?;
        if request.query != "Cypher"
            || request.limit != 5
            || request.conversation_id != Some("project-rig".into())
        {
            return Err(ConversationSearchError::InvalidRequest {
                reason: "unexpected search request".into(),
            });
        }
        Ok(vec![ConversationSearchHit {
            conversation_id: "project-rig".into(),
            chunk_id: "cypher-design".into(),
            start_index: 12,
            messages: vec![Message::user("How should we query a graph database?")],
        }])
    }
}

#[tokio::test]
async fn conversation_search_dispatches_through_facade_toolset() -> anyhow::Result<()> {
    let mut tools = ToolSet::default();
    let name = tools.add_tool(SearchConversationsTool::new(ScopedSearch));
    anyhow::ensure!(name == "search_conversations");
    anyhow::ensure!(tools.tool_definitions().iter().any(|definition| {
        definition.name == name && definition.parameters.get("required") == Some(&json!(["query"]))
    }));

    let result = tools
        .execute(
            &name,
            json!({"query": "Cypher", "conversation_id": "project-rig"}).to_string(),
            &mut ToolContext::default(),
        )
        .await;
    anyhow::ensure!(result.is_success(), "search failed: {:?}", result.error());
    let output = result
        .output()
        .as_json()
        .ok_or_else(|| anyhow::anyhow!("conversation search must return structured JSON"))?;
    let hits: Vec<ConversationSearchHit> = serde_json::from_value(output.clone())?;
    anyhow::ensure!(hits.len() == 1);
    let hit = hits
        .first()
        .ok_or_else(|| anyhow::anyhow!("conversation search must return one hit"))?;
    anyhow::ensure!(hit.conversation_id == "project-rig".into());
    anyhow::ensure!(hit.chunk_id == "cypher-design" && hit.start_index == 12);
    anyhow::ensure!(hit.messages == vec![Message::user("How should we query a graph database?")]);
    Ok(())
}
