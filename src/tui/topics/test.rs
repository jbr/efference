use std::time::Duration;

use serde_json::json;

use super::*;
use crate::api::{CacheMiss, CacheMissReason, RequestUsage, Usage};
use crate::daemon::Session;
use crate::frontend::SubmittedTurn;
use crate::log::{
    ConfigChange, ConfigEdit, EditState, Payload, RequestOrigin, Telemetry, ToolCall,
};

fn decoded(kind: &str, data: Value) -> TopicUpdate {
    TopicUpdate::decode(kind, data)
        .unwrap_or_else(|error| panic!("{kind}: {error}"))
        .unwrap_or_else(|| panic!("{kind} is not a topic the TUI reads"))
}

/// A frame carries the topic's whole value, and the next one replaces it: fields the
/// second leaves out are not kept from the first.
#[test]
fn a_frame_replaces_its_slot_whole() {
    let mut topics = Topics::default();
    assert!(topics.pace.is_none(), "empty before a frame arrives");

    topics.replace(decoded("pace", json!({"speed_ms": 2000, "held": true})));
    assert_eq!(
        topics.pace,
        Some(PaceView {
            speed_ms: 2000,
            held: true
        })
    );
    topics.replace(decoded("pace", json!({"speed_ms": 0, "held": false})));
    assert_eq!(
        topics.pace,
        Some(PaceView {
            speed_ms: 0,
            held: false
        })
    );

    topics.replace(decoded("stall", json!({
        "reason": "retries_exhausted", "attempts": 12, "at": 1, "message": "gave up", "detail": "reset",
    })));
    assert_eq!(topics.stall.as_ref().unwrap().message, "gave up");
    topics.replace(decoded("stall", json!({
        "reason": "request_failed", "attempts": null, "at": 2, "message": "failed", "detail": "",
    })));
    let stall = topics.stall.as_ref().unwrap();
    assert_eq!((stall.message.as_str(), stall.attempts), ("failed", None));
}

/// `null` is the topic's cleared value (docs/control-api.md): it empties the slot.
#[test]
fn null_clears_a_slot() {
    let mut topics = Topics::default();
    topics.replace(decoded(
        "upstream_failure",
        json!({"status": 429, "count": 2, "detail": "slow down"}),
    ));
    assert!(topics.upstream_failure.is_some());
    topics.replace(decoded("upstream_failure", Value::Null));
    assert!(topics.upstream_failure.is_none());
}

/// A topic the TUI does not read is not an error: a host that grows one stays usable.
#[test]
fn a_topic_the_tui_does_not_read_is_skipped() {
    for kind in [
        "queued",
        "notifications",
        "status",
        "a_topic_from_the_future",
    ] {
        assert!(
            matches!(TopicUpdate::decode(kind, json!([])), Ok(None)),
            "{kind}"
        );
    }
}

/// A value that does not deserialize is an error for the caller to drop, and the slot,
/// untouched by it, keeps what it held.
#[test]
fn a_value_that_does_not_deserialize_leaves_the_slot_as_it_was() {
    let mut topics = Topics::default();
    topics.replace(decoded("loop_phase", json!({"phase": "gated"})));
    assert_eq!(topics.loop_phase, LoopPhase::Gated);

    for malformed in [
        json!({"phase": "a_phase_from_the_future"}),
        json!({"speed_ms": 1}),
        json!("gated"),
        Value::Null,
    ] {
        assert!(TopicUpdate::decode("loop_phase", malformed).is_err());
    }
    assert!(TopicUpdate::decode("pace", json!({"speed_ms": "fast"})).is_err());
    assert_eq!(topics.loop_phase, LoopPhase::Gated);
}

/// Every name in the table is a topic the host publishes, and every value a session
/// publishes for one of them is a value the store takes.
#[test]
fn the_store_reads_what_the_host_publishes() {
    let (session, _turns) = Session::in_memory();
    let state: Value = serde_json::from_str(&session.state_json()).unwrap();
    let published = state.as_object().unwrap();

    let mut topics = Topics::default();
    for name in TopicUpdate::NAMES {
        let data = published
            .get(*name)
            .unwrap_or_else(|| panic!("`{name}` is not a topic the host publishes"));
        let update = decoded(name, data.clone());
        assert_eq!(update.carried(), *data, "`{name}` reads back whole");
        topics.replace(update);
    }

    // A fresh session's values, as they came through.
    assert_eq!(topics.pace.unwrap().speed_ms, 0);
    assert_eq!(topics.loop_phase, LoopPhase::Idle);
    assert_eq!(topics.usage.unwrap().cumulative.input_tokens, 0);
    assert_eq!(topics.review_level, Some(ReviewLevel::SpotterHuman));
    assert!(topics.verbs.unwrap().turn.accepted);
    assert!(topics.gate.is_none() && topics.session_identity.is_none());
}

/// What each topic of a busy session reads back as is the value the host sent, field for
/// field: the dials turned, a question holding the turns it took from the queue, a request
/// with an attributed cache miss, a proposal, a running tool and a compaction waiting. A
/// field the host serializes and the store does not read back, or the other way, fails here.
#[test]
fn the_store_holds_what_a_busy_session_publishes_unchanged() {
    let (session, _turns) = Session::in_memory();
    session.set_pace(Some(Duration::from_millis(2_500)), Some(true));
    session.set_review_level(ReviewLevel::HumanOnly);
    session.submit_turn(SubmittedTurn::new("first", "sub-1"));
    session.submit_turn(SubmittedTurn::new("second", "sub-2"));
    let _asking = session.begin_ask("toolu_ask", "Which branch?".into(), 2, 3);
    session.set_phase(LoopPhase::Delayed {
        until: 1_754_300_000_000,
    });
    session.record_request(
        RequestOrigin::Turn,
        RequestUsage::new(Usage {
            input_tokens: 11,
            output_tokens: 21,
            cache_creation_input_tokens: 31,
            cache_read_input_tokens: 0,
        })
        .with_model("claude-haiku-4-5")
        .with_cache_miss(CacheMiss {
            reason: CacheMissReason::ToolsChanged,
            cache_missed_input_tokens: Some(12_280),
        }),
    );
    session
        .append(
            1,
            Payload::ConfigEdit(ConfigEdit::new(
                EditState::Proposed,
                ConfigChange::Path {
                    path: "/tmp/scratch".into(),
                    read: true,
                    write: false,
                },
            )),
        )
        .unwrap();
    session
        .append(
            1,
            Payload::assistant(crate::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![ToolCall::new(
                    "bash",
                    "toolu_1",
                    json!({"command": "cargo test", "intent": "run the suite"}),
                )],
            )),
        )
        .unwrap();
    session
        .append(1, Payload::Telemetry(Telemetry::call_started("toolu_1")))
        .unwrap();
    session
        .append(
            1,
            Payload::Telemetry(Telemetry::CompactionRequested { warm: true, cut: 3 }),
        )
        .unwrap();

    let state: Value = serde_json::from_str(&session.state_json()).unwrap();
    let published = state.as_object().unwrap();
    let mut topics = Topics::default();
    for name in TopicUpdate::NAMES {
        let update = decoded(name, published[*name].clone());
        assert_eq!(
            update.carried(),
            published[*name],
            "`{name}` reads back whole"
        );
        topics.replace(update);
    }

    // The values that make this state a busy one, read through the slots.
    let ask = topics.ask.as_ref().unwrap();
    assert_eq!((ask.n, ask.of, ask.recalled.len()), (2, 3, 2));
    assert_eq!(topics.pace.unwrap().speed_ms, 2_500);
    assert_eq!(topics.review_level, Some(ReviewLevel::HumanOnly));
    assert_eq!(topics.proposals[0].seq, 1);
    let usage = topics.usage.as_ref().unwrap();
    assert_eq!(usage.cumulative.output_tokens, 21);
    assert_eq!(
        usage.last_cache_miss.as_ref().unwrap().reason,
        CacheMissReason::ToolsChanged
    );
    let operations = topics.current_operations.as_ref().unwrap();
    assert_eq!(operations.tools[0].command.as_deref(), Some("cargo test"));
    assert!(operations.compaction.is_some());
}
