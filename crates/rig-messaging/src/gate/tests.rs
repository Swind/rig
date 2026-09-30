use super::*;
use crate::test_support::inbound;

#[test]
fn admission_rules_use_original_metadata() {
    let mut m = inbound("hello");
    let mut gate = Gate::default();
    assert!(gate.allows(&m, "bot"));
    assert!(!gate.allows(&m, "alice-id"));
    m.is_dm = false;
    assert!(!gate.allows(&m, "bot"));
    m.is_thread = true;
    assert!(gate.allows(&m, "bot"));
    assert!(m.message.channel.thread_id.is_none());
    m.is_thread = false;
    m.mentions_bot = true;
    assert!(gate.allows(&m, "bot"));
    gate.allowed_channels = Some(HashSet::from(["other".into()]));
    m.reply_channel.channel_id = "other".into();
    assert!(!gate.allows(&m, "bot"));
    gate.allowed_channels = Some(HashSet::new());
    assert!(gate.allows(&m, "bot"));
    gate.allowed_users = Some(HashSet::from(["other".into()]));
    assert!(!gate.allows(&m, "bot"));
    m.sender.is_bot = true;
    assert!(!gate.allows(&m, "bot"));
    gate.allow_bots = true;
    assert!(gate.allows(&m, "bot"));
    assert!(!gate.allows(&m, "alice-id"));
}
