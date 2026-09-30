//! Runs a Rig agent in Discord DMs and message threads.
//!
//! Requires `DISCORD_BOT_TOKEN` and `OPENAI_API_KEY`, plus Discord's Message
//! Content intent. Replies and history use the destination thread; reactions
//! stay on the original triggering message.

mod discord;
use rig::{
    AgentBuilder,
    memory::InMemoryConversationMemory,
    messaging::{ChatConfig, ChatRouter, Gate},
    providers::openai::{self, OpenAI},
    tool::{Tool, ToolContext},
};
use serenity::all::{Client, GatewayIntents, Http};
use std::{collections::HashSet, sync::Arc};

struct CountCharacters;
impl Tool for CountCharacters {
    const NAME: &'static str = "count_characters";
    type Error = std::io::Error;
    type Args = serde_json::Value;
    type Output = usize;
    fn description(&self) -> String {
        "Count Unicode characters in text".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]})
    }
    async fn call(
        &self,
        _: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let text = args
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "text must be a string")
            })?;
        Ok(text.chars().count())
    }
}
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
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let token = std::env::var("DISCORD_BOT_TOKEN")?;
    let bot_id = Http::new(&token).get_current_user().await?.id;
    let provider = OpenAI::from_env()?;
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| openai::GPT_4O_MINI.into());
    let agent = AgentBuilder::new(provider.completion(model))
        .preamble(
            "You are a helpful assistant. Use count_characters when asked to count characters.",
        )
        .memory(InMemoryConversationMemory::new())
        .default_max_turns(3)
        .tool(CountCharacters)
        .build();
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
    let gate = Gate {
        allowed_channels: allowlist("DISCORD_ALLOWED_CHANNELS"),
        allowed_users: allowlist("DISCORD_ALLOWED_USERS"),
        ..Default::default()
    };
    let router = Arc::new(ChatRouter::new(agent, gate, cfg));
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;
    let mut client = Client::builder(token, intents)
        .event_handler(discord::Handler::new(router, bot_id)?)
        .await?;
    client.start().await?;
    Ok(())
}
