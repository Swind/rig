use super::*;

#[test]
fn session_keys_preserve_field_boundaries() {
    let base = ChannelRef {
        platform: "chat".into(),
        scope_id: None,
        channel_id: "room".into(),
        thread_id: None,
    };
    let variants = [
        base.clone(),
        ChannelRef {
            scope_id: Some(String::new()),
            ..base.clone()
        },
        ChannelRef {
            scope_id: Some("a".into()),
            channel_id: "b:c".into(),
            ..base.clone()
        },
        ChannelRef {
            scope_id: Some("a:b".into()),
            channel_id: "c".into(),
            ..base.clone()
        },
        ChannelRef {
            channel_id: "房間".into(),
            ..base.clone()
        },
        ChannelRef {
            thread_id: Some(String::new()),
            ..base.clone()
        },
    ];
    let keys: std::collections::HashSet<_> = variants.iter().map(ChannelRef::session_key).collect();
    assert_eq!(keys.len(), variants.len());
    assert_eq!(variants[4].session_key(), "v1:4:chat-:6:房間-:");
}

#[test]
fn identical_slack_threads_in_different_channels_are_distinct() {
    let first = ChannelRef {
        platform: "slack".into(),
        scope_id: Some("workspace".into()),
        channel_id: "one".into(),
        thread_id: Some("123.456".into()),
    };
    let second = ChannelRef {
        channel_id: "two".into(),
        ..first.clone()
    };
    assert_ne!(first.session_key(), second.session_key());
}
