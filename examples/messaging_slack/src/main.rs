//! Runs a Rig agent in Slack DMs and message threads over Socket Mode.
//!
//! Requires `SLACK_BOT_TOKEN`, `SLACK_APP_TOKEN` and `OPENAI_API_KEY`.
//! Configure message event subscriptions as described in the example README.

mod slack;

use rig_agent::AgentBuilder;
use rig_core::{
    memory::InMemoryConversationMemory,
    providers::openai::{self, OpenAI},
};
use rig_messaging::{ChatConfig, ChatRouter, Gate};
use std::{collections::HashSet, sync::Arc};

fn allowlist(name: &str) -> Option<HashSet<String>> {
    std::env::var(name).ok().map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = Arc::new(slack::SlackAdapter::new(std::env::var("SLACK_BOT_TOKEN")?)?);
    let identity = adapter.identity().await?;
    let provider = OpenAI::from_env()?;
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| openai::GPT_4O_MINI.into());
    let agent = AgentBuilder::new(provider.completion(model))
        .memory(InMemoryConversationMemory::new())
        .default_max_turns(3)
        .build();
    let gate = Gate {
        allowed_channels: allowlist("SLACK_ALLOWED_CHANNELS"),
        allowed_users: allowlist("SLACK_ALLOWED_USERS"),
        ..Default::default()
    };
    let cfg = ChatConfig {
        attachment_mime_types: [
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
            "application/pdf",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        ..Default::default()
    };
    let router = Arc::new(ChatRouter::new(agent, gate, cfg));
    let handler = slack::Handler::new(adapter, router, identity);
    handler.run(&std::env::var("SLACK_APP_TOKEN")?).await?;
    Ok(())
}
