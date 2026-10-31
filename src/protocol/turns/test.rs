use super::*;

/// The `delivery` a turn's answer carries and the one the `verbs` topic publishes are one
/// spelling: serde's is `as_str`'s, and `parse` reads it back.
#[test]
fn a_landing_is_spelled_one_way() {
    for landing in [
        Landing::Answered,
        Landing::Now,
        Landing::Queued,
        Landing::Held,
    ] {
        assert_eq!(
            serde_json::to_value(landing).unwrap(),
            serde_json::Value::from(landing.as_str())
        );
        assert_eq!(Landing::parse(landing.as_str()), Some(landing));
    }
}
