//! Admission rules for normalized platform input.
//!
//! ```
//! use rig_messaging::Gate;
//! let gate = Gate::default();
//! assert!(!gate.allow_bots);
//! ```

use crate::Inbound;
use std::collections::HashSet;

/// Admission rules. Missing or empty allowlists impose no restriction.
#[derive(Debug, Clone, Default)]
pub struct Gate {
    /// Allowed original channel ids. Thread channels must be listed explicitly.
    pub allowed_channels: Option<HashSet<String>>,
    /// Allowed human sender ids. Other bots are exempt from this list.
    pub allowed_users: Option<HashSet<String>>,
    /// Whether messages from other bots may be admitted.
    pub allow_bots: bool,
}

impl Gate {
    /// Admit input using the original message address and the platform's bot id.
    /// Channel messages outside threads require a mention; DMs and threads do not.
    pub fn allows(&self, m: &Inbound, bot_user_id: &str) -> bool {
        let contains = |list: &Option<HashSet<String>>, id: &str| {
            list.as_ref()
                .is_none_or(|ids| ids.is_empty() || ids.contains(id))
        };
        m.sender.id != bot_user_id
            && (!m.sender.is_bot || self.allow_bots)
            && contains(&self.allowed_channels, &m.message.channel.channel_id)
            && (m.sender.is_bot || contains(&self.allowed_users, &m.sender.id))
            && (m.is_dm || m.is_thread || m.mentions_bot)
    }
}

#[cfg(test)]
mod tests;
