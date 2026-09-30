#![cfg(not(target_family = "wasm"))]
#![cfg_attr(test, allow(clippy::indexing_slicing))]
//! Native messaging transports and conversation routing for Rig agents.
//!
//! Platform ingress normalizes messages into [`Inbound`]. Implement [`ChatAdapter`]
//! to send replies through a platform API. This crate supports native targets only.
//!
//! ```
//! use rig_messaging::ChannelRef;
//! let channel = ChannelRef {
//!     platform: "stdio".into(), scope_id: None,
//!     channel_id: "local".into(), thread_id: None,
//! };
//! assert_eq!(channel.session_key(), "v1:5:stdio-:5:local-:");
//! ```

pub mod adapter;
pub mod types;

pub use adapter::{ChatAdapter, ChatError};
pub use types::{Attachment, AttachmentSource, ChannelRef, Inbound, MessageRef, Sender};
