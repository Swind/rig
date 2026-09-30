//! Platform-neutral message addresses and normalized input.
//!
//! ```
//! use rig_messaging::ChannelRef;
//! let ch = ChannelRef { platform: "chat".into(), scope_id: None,
//!     channel_id: "room".into(), thread_id: None };
//! assert_eq!(ch.session_key(), "v1:4:chat-:4:room-:");
//! ```

/// A platform channel and optional scope and thread.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelRef {
    /// Platform identifier.
    pub platform: String,
    /// Workspace, guild or tenant identifier.
    pub scope_id: Option<String>,
    /// Channel identifier, including Discord thread channels.
    pub channel_id: String,
    /// Thread identifier when threads are nested inside channels.
    pub thread_id: Option<String>,
}

impl ChannelRef {
    /// Encode the address as a stable conversation id with UTF-8 byte lengths.
    /// Absent fields and present empty fields produce different keys.
    pub fn session_key(&self) -> String {
        let mut key = String::from("v1:");
        for field in [
            Some(self.platform.as_str()),
            self.scope_id.as_deref(),
            Some(self.channel_id.as_str()),
            self.thread_id.as_deref(),
        ] {
            match field {
                Some(value) => {
                    key.push_str(&value.len().to_string());
                    key.push(':');
                    key.push_str(value);
                }
                None => key.push_str("-:"),
            }
        }
        key
    }
}

/// An existing platform message and its original address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRef {
    /// Channel containing the message.
    pub channel: ChannelRef,
    /// Platform message identifier.
    pub message_id: String,
}

/// Stable sender identity and display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sender {
    /// Platform user identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Whether the platform marks this sender as a bot.
    pub is_bot: bool,
}

/// Normalized input retaining the original reaction target and reply destination.
#[derive(Debug, Clone)]
pub struct Inbound {
    /// Original triggering message, used for reactions and admission checks.
    pub message: MessageRef,
    /// Outbound destination, also used for memory and locking.
    pub reply_channel: ChannelRef,
    /// Sender identity.
    pub sender: Sender,
    /// User text.
    pub text: String,
    /// Attached media.
    pub attachments: Vec<Attachment>,
    /// Whether the original message is a direct message.
    pub is_dm: bool,
    /// Whether the original message arrived inside a thread.
    pub is_thread: bool,
    /// Whether the original message mentions the bot.
    pub mentions_bot: bool,
}

/// Attachment metadata and a deferred or already downloaded payload.
#[derive(Debug, Clone)]
pub struct Attachment {
    /// Original filename.
    pub filename: String,
    /// MIME type.
    pub mime: String,
    /// Size in bytes, if known.
    pub size: Option<u64>,
    /// Payload location.
    pub source: AttachmentSource,
}

/// Attachment payload or URL supplied by platform ingress.
#[derive(Debug, Clone)]
pub enum AttachmentSource {
    /// Already downloaded bytes.
    Bytes(bytes::Bytes),
    /// Remote media address. Providers may need an accessible URL.
    Url(String),
}

#[cfg(test)]
mod tests;
