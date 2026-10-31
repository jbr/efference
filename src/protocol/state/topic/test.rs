use super::*;

/// The topic set is the contract a client writes its state store against; pin the wire
/// names so a rename has to be deliberate.
#[test]
fn topic_wire_names_are_pinned() {
    let names: Vec<&str> = [
        Topic::Gate,
        Topic::Processes,
        Topic::Usage,
        Topic::Role,
        Topic::Proposals,
        Topic::Notifications,
        Topic::Pace,
        Topic::ReviewLevel,
        Topic::LoopPhase,
        Topic::Verbs,
        Topic::Stall,
        Topic::UpstreamFailure,
        Topic::UpstreamRetry,
        Topic::Cold,
        Topic::CompactionFailure,
        Topic::SessionIdentity,
        Topic::CurrentOperations,
    ]
    .iter()
    .map(|t| t.as_str())
    .collect();
    assert_eq!(
        names,
        [
            "gate",
            "processes",
            "usage",
            "role",
            "proposals",
            "notifications",
            "pace",
            "review_level",
            "loop_phase",
            "verbs",
            "stall",
            "upstream_failure",
            "upstream_retry",
            "cold",
            "compaction_failure",
            "session_identity",
            "current_operations",
        ]
    );
}
