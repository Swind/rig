//! The OpenCode Go dialect of the Messages wire's history suite. The
//! gateway names no model of its own, so the suite runs on opaque ids.

use rig_core::providers::anthropic::wire::OPENCODE_GO;

use super::anthropic::MessagesHistory;

pub const OPENCODE_GO_MESSAGES_HISTORY: MessagesHistory = MessagesHistory {
    dialect: &OPENCODE_GO,
    model: "deepseek-v4.1-flash",
    other_model: "deepseek-v4-flash",
    text_only_model: None,
    signature: "sig_1",
    hosted: false,
};

rig_history_conformance::history_conformance_suite! {
    wire: "anthropic_opencode-go",
    fixture: OPENCODE_GO_MESSAGES_HISTORY,
}
