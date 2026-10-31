//! The `verbs` topic against the routes it describes (docs/control-api.md): for every
//! verb, the route refuses exactly when the published table says it will, with the
//! table's message, across the loop's phases, what is in flight, and the routes' own
//! checks.

use super::*;
use crate::daemon::compact::NotCompactable;
use crate::daemon::{BlockedReason, LoopPhase};
use crate::frontend::SubmittedTurn;
use serde_json::json;

fn session() -> Arc<Session> {
    Session::new(std::env::current_dir().unwrap(), Log::in_memory()).0
}

fn assistant(text: &str) -> Payload {
    Payload::Assistant {
        output: crate::log::AssistantOutput::from_legacy_messages(
            vec![],
            vec![ContentBlock::Text(text.into())],
            vec![],
        ),
        excluded: None,
    }
}

/// A session whose last turn is unanswered: the idle loop has a continue in it.
fn unanswered() -> Arc<Session> {
    let shared = session();
    shared.append(1, user("go")).unwrap();
    shared
}

/// A session with history enough to compact and a cache lifetime; `cold` puts its last
/// request past that lifetime.
fn compactable(cold: bool) -> Arc<Session> {
    let shared = session();
    shared.set_cache_ttl(Duration::from_secs(if cold { 0 } else { 3_600 }));
    append_compactable_history(&shared);
    shared.record_request(RequestOrigin::Turn, RequestUsage::new(Usage::default()));
    if cold {
        std::thread::sleep(Duration::from_millis(5));
        assert!(shared.cold_since().is_some());
    }
    shared
}

fn in_phase(shared: Arc<Session>, phase: LoopPhase) -> Arc<Session> {
    shared.set_phase(phase);
    shared
}

fn gated(shared: Arc<Session>) -> Arc<Session> {
    shared.set_phase(LoopPhase::Executing);
    let call = bash_call("ls");
    std::mem::forget(shared.register_pending(&call, None, None));
    shared
}

fn asking(shared: Arc<Session>) -> Arc<Session> {
    shared.set_phase(LoopPhase::Executing);
    std::mem::forget(shared.begin_ask("ask_1", "Which?".into(), 1, 1));
    shared
}

fn held_triage(shared: Arc<Session>) -> Arc<Session> {
    shared.set_stall(agent::stalled(
        StallReason::TriageUnavailable,
        None,
        "triage could not reach its model".into(),
        "429".into(),
    ));
    in_phase(
        shared,
        LoopPhase::Blocked {
            reason: BlockedReason::TriageUnavailable,
        },
    )
}

fn retrying(shared: Arc<Session>) -> Arc<Session> {
    shared.set_phase(LoopPhase::Requesting);
    shared.upstream_retry.set(agent::stream_abort_retry(
        crate::api::RetrySurface::Turn,
        1,
        12,
        Duration::from_secs(1),
        "503",
    ));
    shared
}

fn queued(shared: Arc<Session>) -> Arc<Session> {
    shared.submit_turn(SubmittedTurn::new("next", "s-q"));
    shared
}

fn compacting(shared: Arc<Session>) -> Arc<Session> {
    assert_eq!(shared.request_compaction(true), Ok(()));
    shared
}

/// A named state, built fresh for each route call.
type State = (&'static str, Box<dyn Fn() -> Arc<Session>>);

/// Every state the table is checked in.
fn states() -> Vec<State> {
    let pace_held = LoopPhase::Blocked {
        reason: BlockedReason::PaceHeld,
    };
    let config_repair = LoopPhase::Blocked {
        reason: BlockedReason::ConfigRepair,
    };
    vec![
        ("empty", Box::new(session)),
        ("unanswered", Box::new(unanswered)),
        (
            "answered",
            Box::new(|| {
                let shared = unanswered();
                shared.append(1, assistant("done")).unwrap();
                shared
            }),
        ),
        ("queued at idle", Box::new(|| queued(unanswered()))),
        (
            "requesting",
            Box::new(|| in_phase(unanswered(), LoopPhase::Requesting)),
        ),
        (
            "executing",
            Box::new(|| in_phase(compactable(true), LoopPhase::Executing)),
        ),
        (
            "pace delay",
            Box::new(|| in_phase(unanswered(), LoopPhase::Delayed { until: u64::MAX })),
        ),
        (
            "pace held",
            Box::new(move || in_phase(compactable(true), pace_held)),
        ),
        (
            "config repair",
            Box::new(move || in_phase(unanswered(), config_repair)),
        ),
        ("retrying", Box::new(|| retrying(unanswered()))),
        ("gated", Box::new(|| gated(compactable(true)))),
        ("asking", Box::new(|| asking(compactable(true)))),
        (
            "cold parked",
            Box::new(|| in_phase(compactable(true), LoopPhase::ColdParked)),
        ),
        (
            "held triage, warm",
            Box::new(|| held_triage(compactable(false))),
        ),
        ("idle, warm", Box::new(|| compactable(false))),
        ("idle, cold", Box::new(|| compactable(true))),
        ("compacting", Box::new(|| compacting(compactable(false)))),
        (
            "compacting while running",
            Box::new(|| in_phase(compacting(compactable(false)), LoopPhase::Requesting)),
        ),
    ]
}

fn verbs(shared: &Session) -> Value {
    shared.topic_json(Topic::Verbs)
}

#[test]
fn the_continue_route_refuses_exactly_when_the_table_says() {
    for (name, make) in states() {
        let shared = make();
        let table = verbs(&shared)["continue"].clone();
        let expected = match shared.request_continue(Default::default()) {
            Ok(()) => json!({ "accepted": true }),
            Err(why) => json!({ "accepted": false, "message": why.message() }),
        };
        assert_eq!(table, expected, "{name}");
    }
}

#[test]
fn the_compact_route_refuses_exactly_when_the_table_says() {
    for (name, make) in states() {
        let table = verbs(&make())["compact"].clone();
        let without_warm = make().request_compaction(false);
        let with_warm = make().request_compaction(true);
        match (table["accepted"].as_bool(), table["warm"].as_bool()) {
            (Some(true), Some(false)) => {
                assert_eq!(without_warm, Ok(()), "{name}");
                assert_eq!(with_warm, Ok(()), "{name}");
            }
            (Some(true), Some(true)) => {
                assert_eq!(without_warm, Err(NotCompactable::NotCold), "{name}");
                assert_eq!(with_warm, Ok(()), "{name}");
            }
            (Some(false), None) => {
                let refusal = with_warm.expect_err(name);
                assert_eq!(table["message"], refusal.message(), "{name}");
                assert!(without_warm.is_err(), "{name}");
            }
            _ => panic!("{name}: malformed entry {table}"),
        }
    }
}

/// A turn is always accepted; its entry says whether it lands now or waits in the queue.
#[test]
fn a_turn_is_accepted_and_says_when_it_lands() {
    let expected = [
        ("empty", "now"),
        ("queued at idle", "queued"),
        ("requesting", "queued"),
        ("executing", "queued"),
        ("pace delay", "now"),
        ("pace held", "now"),
        ("config repair", "queued"),
        ("retrying", "queued"),
        ("gated", "queued"),
        ("asking", "now"),
        ("cold parked", "now"),
        ("held triage, warm", "queued"),
    ];
    let states = states();
    for (name, delivery) in expected {
        let (_, make) = states.iter().find(|(n, _)| *n == name).unwrap();
        assert_eq!(
            verbs(&make())["turn"],
            json!({ "accepted": true, "delivery": delivery }),
            "{name}"
        );
    }
}

/// The pause between upstream attempts reads `delayed` as a pace delay does, but a submit
/// does not end it: a turn sent during it waits in the queue.
#[tokio::test]
async fn a_turn_during_a_retry_pause_is_queued() {
    let shared = unanswered();
    shared.set_phase(LoopPhase::Requesting);
    let retry = Arc::clone(&shared.upstream_retry);
    let mut pause = std::pin::pin!(retry.delay(Duration::from_secs(60)));
    assert_eq!(futures_lite::future::poll_once(pause.as_mut()).await, None);
    assert!(matches!(shared.loop_phase(), LoopPhase::Delayed { .. }));
    assert_eq!(verbs(&shared)["turn"]["delivery"], "queued");
}
