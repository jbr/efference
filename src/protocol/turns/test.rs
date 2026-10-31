use super::*;

/// The `delivery` a turn's answer carries and the one the `verbs` topic publishes are one
/// spelling: serde's, which `parse` reads back.
#[test]
fn a_landing_is_spelled_one_way() {
    for landing in [
        Landing::Answered,
        Landing::Now,
        Landing::Queued,
        Landing::Held,
    ] {
        let word = serde_json::to_value(landing).unwrap();
        assert_eq!(Landing::parse(word.as_str().unwrap()), Some(landing));
    }
}
