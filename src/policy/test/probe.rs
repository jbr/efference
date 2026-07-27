//! Prefix-promotion probe descent and the vote ledger.

use super::*;

fn toks(command: &str) -> Vec<String> {
    simple_command_tokens(command).expect("lexes")
}

/// The frequency gate: a prefix isn't probed until it's been seen `MIN_SIGHTINGS_TO_PROBE` times.
#[test]
fn descent_waits_for_the_frequency_gate() {
    let role = Config::default(); // empty allowlist → every prefix is promotable
    let mut ledger = ProbeLedger::default();
    let t = toks("npm install");
    ledger.observe(&t); // 1st sighting
    assert_eq!(
        ledger.next_probe(&role, &t),
        None,
        "one sighting is too few"
    );
    ledger.observe(&t); // 2nd sighting → now eligible
    assert_eq!(ledger.next_probe(&role, &t).as_deref(), Some("npm"));
}

/// The descent picks the shortest unresolved prefix, then descends past a not-safe vote.
#[test]
fn descent_picks_shortest_then_descends_past_a_vote() {
    let role = Config::default();
    let mut ledger = ProbeLedger::default();
    let t = toks("git log --oneline");
    // Warm all prefixes past the gate.
    ledger.observe(&t);
    ledger.observe(&t);
    // Shortest first: `git`.
    assert_eq!(ledger.next_probe(&role, &t).as_deref(), Some("git"));
    // A not-safe vote resolves `git`; the descent moves to the next prefix, `git log`.
    ledger.vote("git", false);
    assert_eq!(ledger.next_probe(&role, &t).as_deref(), Some("git log"));
    // A safe vote on `git log` resolves it too (and flags it ready to promote).
    ledger.vote("git log", true);
    assert!(ledger.ready_to_promote("git log"));
    assert!(!ledger.ready_to_promote("git"));
    // `git log --oneline` was never warmed on its own past the gate independently... it was
    // (every observe bumps all prefixes), so it's the last eligible one.
    assert_eq!(
        ledger.next_probe(&role, &t).as_deref(),
        Some("git log --oneline")
    );
}

/// Coverage is monotonic by token-prefix: if `cargo` is allowlisted, every `cargo …` prefix is
/// covered too (and the command would fast-path clear, never reaching the spotter). The descent's
/// coverage check is a defensive guard — when all prefixes are covered it probes nothing.
#[test]
fn descent_probes_nothing_when_every_prefix_is_covered() {
    let role = Config {
        commands: commands(&["cargo"]),
        ..Default::default()
    };
    let mut ledger = ProbeLedger::default();
    let t = toks("cargo test --workspace");
    ledger.observe(&t);
    ledger.observe(&t);
    assert_eq!(ledger.next_probe(&role, &t), None);
}

/// The fold reconstructs seen-counts (from each `Spot`'s command) and votes (from probed `Spot`s,
/// model verdicts only) so a resumed daemon picks up exactly where it left off.
#[test]
fn probe_ledger_folds_seen_and_votes_from_the_log() {
    use crate::log::{CallId, Origin, Spot, Stance};

    // Two `cargo test` invocations, the second carrying a `clear_always` probe vote for `cargo`.
    fn assistant(id: &str, command: &str) -> LogEntry {
        LogEntry {
            seq: 0,
            turn: 0,
            at: Timestamp(0),
            payload: Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId(id.into()),
                    name: bash::NAME.into(),
                    input: serde_json::json!({ "command": command }),
                }],
            },
        }
    }
    fn spot(id: &str, stance: Stance, probed: Option<&str>) -> LogEntry {
        LogEntry {
            seq: 0,
            turn: 0,
            at: Timestamp(0),
            payload: Payload::Spot(Spot {
                call_id: CallId(id.into()),
                stance,
                reason: String::new(),
                origin: Origin::Model,
                probed_prefix: probed.map(str::to_string),
            }),
        }
    }

    let log = [
        assistant("a", "cargo test"),
        spot("a", Stance::Clear, Some("cargo")), // 1st sighting, not-safe-yet vote? no — clear, not clear_always
        assistant("b", "cargo test"),
        spot("b", Stance::ClearAlways, Some("cargo")), // 2nd sighting + safe vote
    ];
    let ledger = probe_ledger(&log);
    // Two sightings of `cargo` and `cargo test`.
    let role = Config::default();
    // `cargo` got one not-safe (Clear) and one safe (ClearAlways) vote → resolved either way;
    // and it reached the safe threshold, so it's ready to promote.
    assert!(ledger.ready_to_promote("cargo"));
    // Resolved → not re-probed even though seen ≥ 2.
    assert_eq!(
        ledger.next_probe(&role, &toks("cargo test")).as_deref(),
        Some("cargo test")
    );
}

/// A fail-closed (`Fallback`) spotter verdict casts no vote — it didn't actually judge.
#[test]
fn probe_ledger_ignores_fallback_verdicts() {
    use crate::log::{CallId, Origin, Spot, Stance};
    let log = [LogEntry {
        seq: 0,
        turn: 0,
        at: Timestamp(0),
        payload: Payload::Spot(Spot {
            call_id: CallId("x".into()),
            stance: Stance::ClearAlways, // would be a safe vote — but origin is Fallback
            reason: String::new(),
            origin: Origin::Fallback,
            probed_prefix: Some("cargo".into()),
        }),
    }];
    let ledger = probe_ledger(&log);
    assert!(
        !ledger.ready_to_promote("cargo"),
        "a fallback casts no vote"
    );
}
