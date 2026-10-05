//! Built-in tools shipped with `rig` that agents can use out of the box.

pub mod search_conversations;
pub mod think;
pub use search_conversations::SearchConversationsTool;
pub use think::ThinkTool;
