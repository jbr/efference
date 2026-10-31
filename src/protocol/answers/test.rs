use super::*;

/// Every word a route answers `status` with, and the body that carries it
/// (docs/control-api.md). Two of the routes, the push subscription and its removal, write
/// a machine-scoped store a test cannot reach, so their words are pinned here.
#[test]
fn every_status_word_keeps_its_bytes() {
    for (word, bytes) in [
        (StatusWord::Accepted, r#"{"status":"accepted"}"#),
        (StatusWord::Resolved, r#"{"status":"resolved"}"#),
        (StatusWord::Escalated, r#"{"status":"escalated"}"#),
        (StatusWord::Interrupted, r#"{"status":"interrupted"}"#),
        (StatusWord::Continuing, r#"{"status":"continuing"}"#),
        (StatusWord::Compacting, r#"{"status":"compacting"}"#),
        (StatusWord::Killed, r#"{"status":"killed"}"#),
        (StatusWord::Subscribed, r#"{"status":"subscribed"}"#),
        (StatusWord::Unsubscribed, r#"{"status":"unsubscribed"}"#),
        (StatusWord::Pending, r#"{"status":"pending"}"#),
    ] {
        let body = StatusBody::new(word);
        assert_eq!(serde_json::to_string(&body).unwrap(), bytes);
        assert_eq!(serde_json::from_str::<StatusBody>(bytes).unwrap(), body);
    }
}
