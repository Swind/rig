use qdrant_client::Qdrant;
use rig_conversation_store::{ConversationStore, StoreConfig};
use rig_core::{
    completion::Message,
    conversation_search::ConversationSearchRequest,
    memory::ConversationMemory,
    providers::openai::{self, OpenAI},
    tool::{PortableTool, builtin::SearchConversationsTool},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let provider = OpenAI::from_env()?;
    let model = provider
        .embedding(openai::TEXT_EMBEDDING_ADA_002, None)
        .erase();
    let store = ConversationStore::open(
        "conversations.sqlite",
        Qdrant::from_url("http://localhost:6334").build()?,
        "rig-conversation-chunks",
        model,
        StoreConfig::new("example-workspace", openai::TEXT_EMBEDDING_ADA_002, 1536),
    )
    .await?;
    let id = "example-thread".into();
    store
        .append(
            &id,
            vec![
                Message::user("Rig supports parameterized Cypher searches."),
                Message::assistant(
                    "Original conversations can be found through vector search and nearby context.",
                ),
            ],
        )
        .await?;
    store.process_pending(20).await?;

    let tool = SearchConversationsTool::new(store.clone());
    let excerpts = tool
        .call(ConversationSearchRequest {
            query: "How do I search conversations using Cypher?".into(),
            limit: 5,
            conversation_id: Some(id.clone()),
        })
        .await?;
    println!("{}", serde_json::to_string_pretty(&excerpts)?);

    let _agent = rig_agent::agent::AgentBuilder::new(provider.chat("gpt-4o-mini"))
        .memory(store)
        .tool(tool)
        .build();
    Ok(())
}
