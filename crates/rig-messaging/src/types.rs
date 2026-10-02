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

/// Optional display context supplied by platform ingress.
///
/// Names and mentions are descriptive data. Routing, authorization, and memory keys use IDs.
#[derive(Debug, Clone, Default)]
pub struct MessageContext {
    /// Display name of the original message's channel, when available.
    pub channel_name: Option<String>,
    /// Original message time normalized to UTC, when supplied by the platform.
    pub sent_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Mentioned users with their platform IDs and available display names.
    pub mentions: Vec<Sender>,
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
    /// Display context included in the model's user message.
    pub context: MessageContext,
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

impl Inbound {
    /// Render platform, channel, sender, optional UTC time and mentions before the message text.
    ///
    /// Missing names fall back to stable IDs. Metadata stays on separate header lines while the
    /// original text is preserved. This representation is user content, not authorization data.
    pub fn prompt_text(&self) -> String {
        let channel = &self.message.channel;
        let mut lines = vec![
            format!("Platform: {}", header_value(&channel.platform)),
            format!(
                "Channel: {}",
                display_identity(self.context.channel_name.as_deref(), &channel.channel_id)
            ),
            format!(
                "Sender: {}",
                display_identity(Some(&self.sender.name), &self.sender.id)
            ),
        ];
        if let Some(sent_at) = self.context.sent_at {
            lines.push(format!(
                "Time: {}",
                sent_at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
            ));
        }
        if !self.context.mentions.is_empty() {
            let mut seen = std::collections::HashSet::new();
            let mentions = self
                .context
                .mentions
                .iter()
                .filter(|sender| seen.insert(sender.id.as_str()))
                .map(|sender| display_identity(Some(&sender.name), &sender.id))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Mentions: {mentions}"));
        }
        format!("{}\n\n{}", lines.join("\n"), self.text)
    }
}

fn display_identity(name: Option<&str>, id: &str) -> String {
    match name
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != id)
    {
        Some(name) => format!("{} ({})", header_value(name), header_value(id)),
        None => header_value(id),
    }
}

fn header_value(value: &str) -> String {
    value
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
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
