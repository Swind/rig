//! Runs a Rig agent in Slack DMs and message threads over Socket Mode.
//!
//! Requires Slack bot/app tokens and an OpenAI or OpenCode Go API key.
//! Configure message event subscriptions as described in the example README.

mod provider;
mod slack;

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
    if let Err(error) = dotenvy::dotenv()
        && !error.not_found()
    {
        return Err(std::io::Error::other("failed to load .env").into());
    }
    let (agent, go) = provider::agent_from_env()?;
    let adapter = Arc::new(slack::SlackAdapter::new(std::env::var("SLACK_BOT_TOKEN")?)?);
    let identity = adapter.identity().await?;
    let gate = Gate {
        allowed_channels: allowlist("SLACK_ALLOWED_CHANNELS"),
        allowed_users: allowlist("SLACK_ALLOWED_USERS"),
        ..Default::default()
    };
    let cfg = ChatConfig {
        attachment_mime_types: if go {
            HashSet::new()
        } else {
            [
                "image/png",
                "image/jpeg",
                "image/gif",
                "image/webp",
                "application/pdf",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        },
        ..Default::default()
    };
    let router = Arc::new(ChatRouter::new(agent, gate, cfg));
    let handler = slack::Handler::new(adapter, router, identity);
    handler.run(&std::env::var("SLACK_APP_TOKEN")?).await?;
    Ok(())
}
