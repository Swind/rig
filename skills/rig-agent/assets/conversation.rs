//! Two model calls sharing one process-local conversation.
//! Requires OPENAI_API_KEY. OPENAI_MODEL selects the completion model.

use rig_agent::{AgentBuilder, run::response::MemoryAppend};
use rig_core::{
    memory::InMemoryConversationMemory,
    providers::openai::{self, OpenAI},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let provider = OpenAI::from_env()?;
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| openai::GPT_4O_MINI.into());
    let agent = AgentBuilder::new(provider.completion(model))
        .preamble("Answer briefly. Use conversation history when relevant.")
        .memory(InMemoryConversationMemory::new())
        .build();

    for prompt in [
        "I am learning Rust.",
        "Which language did I say I am learning?",
    ] {
        let response = agent
            .prompt(prompt)
            .conversation("demo-conversation")
            .max_turns(2)
            .run()
            .await?;
        println!("{}", response.output);
        if let Some(MemoryAppend::Failed { report }) = response.memory_append {
            eprintln!("Conversation persistence failed: {report}");
            break;
        }
    }
    Ok(())
}
