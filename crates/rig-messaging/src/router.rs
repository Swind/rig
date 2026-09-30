//! Conversation routing with serialized history updates.
//!
//! ```
//! use rig_messaging::{ChatConfig, ChatRouter, Gate};
//! use rig_agent::Agent;
//! fn router(agent_with_memory: Agent) -> ChatRouter {
//!     ChatRouter::new(agent_with_memory, Gate::default(), ChatConfig::default())
//! }
//! ```

use crate::{AttachmentSource, ChatAdapter, ChatError, Gate, Inbound, markdown::TableMode};
use rig_agent::Agent;
use rig_core::message::{MediaType, Message, MimeType, UserContent};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::Mutex;

/// Output rendering and media types accepted by the configured provider/model.
#[derive(Debug, Clone, Default)]
pub struct ChatConfig {
    /// Table rendering used when the adapter cannot render native tables.
    pub table_mode: TableMode,
    /// MIME types accepted by the model for both bytes and URLs.
    /// Empty by default. Other attachments become short text notes.
    pub attachment_mime_types: HashSet<String>,
}

/// Routes inputs through one Agent with a mutex per conversation.
/// The Agent must be configured with `.memory(...)` or `.memory_handler(...)`.
/// Use one router for all ingress sharing that Agent and memory backend.
pub struct ChatRouter {
    agent: Agent,
    gate: Gate,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    cfg: ChatConfig,
}

impl ChatRouter {
    /// Construct a router. The supplied Agent must have conversation memory configured.
    pub fn new(agent: Agent, gate: Gate, cfg: ChatConfig) -> Self {
        Self {
            agent,
            gate,
            locks: Mutex::new(HashMap::new()),
            cfg,
        }
    }

    /// Check admission before ingress creates threads or downloads attachments.
    pub fn allows(&self, m: &Inbound, bot_user_id: &str) -> bool {
        self.gate.allows(m, bot_user_id)
    }

    /// Process an admitted input, returning stream, memory or delivery failures.
    /// Rejected inputs return success without platform operations. Concurrent turns
    /// execute in mutex acquisition order, which may differ from arrival order.
    pub async fn handle(
        &self,
        adapter: Arc<dyn ChatAdapter>,
        m: Inbound,
        bot_user_id: &str,
    ) -> Result<(), ChatError> {
        if !self.allows(&m, bot_user_id) {
            return Ok(());
        }
        if adapter.message_limit() == 0 {
            return Err(ChatError::InvalidMessageLimit);
        }
        let key = m.reply_channel.session_key();
        let lock = {
            let mut locks = self.locks.lock().await;
            locks.entry(key.clone()).or_default().clone()
        };
        let guard = lock.lock().await;
        let prompt = build_prompt(&m, &self.cfg);
        let stream = self.agent.prompt(prompt).conversation(key.clone()).stream();
        let result =
            crate::egress::egress(adapter.as_ref(), &m.reply_channel, stream, &self.cfg).await;
        drop(guard);
        let mut locks = self.locks.lock().await;
        drop(lock);
        if locks
            .get(&key)
            .is_some_and(|entry| Arc::strong_count(entry) == 1)
        {
            locks.remove(&key);
        }
        result
    }
}

fn build_prompt(m: &Inbound, cfg: &ChatConfig) -> Message {
    let mut content = vec![UserContent::text(format!(
        "[{} ({})]\n{}",
        m.sender.name, m.sender.id, m.text
    ))];
    for attachment in &m.attachments {
        let media = if cfg.attachment_mime_types.contains(&attachment.mime) {
            MediaType::from_mime_type(&attachment.mime)
        } else {
            None
        };
        let item = match (media, &attachment.source) {
            (Some(MediaType::Image(mt)), AttachmentSource::Bytes(data)) => {
                UserContent::image_raw(data.to_vec(), Some(mt), None)
            }
            (Some(MediaType::Image(mt)), AttachmentSource::Url(url)) => {
                UserContent::image_url(url.clone(), Some(mt), None)
            }
            (Some(MediaType::Document(mt)), AttachmentSource::Bytes(data)) => {
                UserContent::document_raw(data.to_vec(), Some(mt))
            }
            (Some(MediaType::Document(mt)), AttachmentSource::Url(url)) => {
                UserContent::document_url(url.clone(), Some(mt))
            }
            (Some(MediaType::Audio(mt)), AttachmentSource::Bytes(data)) => {
                UserContent::audio_raw(data.to_vec(), Some(mt))
            }
            (Some(MediaType::Audio(mt)), AttachmentSource::Url(url)) => {
                UserContent::audio_url(url.clone(), Some(mt))
            }
            (Some(MediaType::Video(mt)), AttachmentSource::Bytes(data)) => {
                UserContent::video_raw(data.to_vec(), Some(mt))
            }
            (Some(MediaType::Video(mt)), AttachmentSource::Url(url)) => {
                UserContent::video_url(url.clone(), Some(mt))
            }
            _ => UserContent::text(format!(
                "[Attachment unavailable: {} ({})]",
                attachment.filename, attachment.mime
            )),
        };
        content.push(item);
    }
    Message::User { content }
}

#[cfg(test)]
mod tests;
