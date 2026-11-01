use super::*;
use crate::record::log::{Keep, RawResult, Triaged};
use crate::record::persist::Log;
use crate::record::project::project;

#[test]
fn captured_output_stays_off_wire_until_a_verdict() {
    let mut log = Log::in_memory();
    log.append(0, Payload::user("task")).unwrap();
    let result = log
        .append(
            1,
            Payload::tool_result(
                "call",
                RawResult::text("selected\nprivate"),
                WireState::Pending,
            ),
        )
        .unwrap();
    let before = serde_json::to_string(&project(log.entries())).unwrap();
    assert!(!before.contains("selected"));
    assert!(!before.contains("private"));
    assert!(result_verdicts(log.entries()).is_empty());

    log.append(
        1,
        Payload::result_verdict(
            result,
            WireState::Triaged(Triaged::new(Keep::Ranges(vec![(1, 1)]))),
        ),
    )
    .unwrap();
    let after = serde_json::to_string(&project(log.entries())).unwrap();
    assert!(after.contains("selected"));
    assert!(!after.contains("private"));
    assert!(matches!(
        log.entries()[result as usize].payload,
        Payload::ToolResult {
            wire: WireState::Pending,
            ..
        }
    ));
    assert_eq!(
        after,
        serde_json::to_string(&project(log.entries())).unwrap()
    );
}

#[test]
fn later_verdict_cannot_change_an_accepted_prefix() {
    let mut log = Log::in_memory();
    let result = log
        .append(
            0,
            Payload::tool_result("call", RawResult::text("output"), WireState::Pending),
        )
        .unwrap();
    log.append(0, Payload::result_verdict(result, WireState::Raw))
        .unwrap();
    let accepted = serde_json::to_string(&project(log.entries())).unwrap();
    log.append(0, Payload::result_verdict(result, WireState::Elided))
        .unwrap();
    assert_eq!(
        accepted,
        serde_json::to_string(&project(log.entries())).unwrap()
    );
}

#[test]
fn verdicts_only_resolve_prior_pending_results() {
    let mut log = Log::in_memory();
    log.append(0, Payload::result_verdict(1, WireState::Raw))
        .unwrap();
    log.append(
        0,
        Payload::tool_result("pending", RawResult::text("pending"), WireState::Pending),
    )
    .unwrap();
    let finalized = log
        .append(
            0,
            Payload::tool_result("final", RawResult::text("final"), WireState::Raw),
        )
        .unwrap();
    log.append(0, Payload::result_verdict(finalized, WireState::Elided))
        .unwrap();
    log.append(0, Payload::result_verdict(1, WireState::Pending))
        .unwrap();
    assert!(result_verdicts(log.entries()).is_empty());
    let projected = serde_json::to_string(&project(log.entries())).unwrap();
    assert!(projected.contains("final"));
    assert!(!projected.contains("pending"));
}
