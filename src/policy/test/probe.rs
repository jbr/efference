//! Prefix-promotion probe descent and the vote ledger.

use super::*;
use crate::config::testing::ConfigFixture;

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
    let role = ConfigFixture::new()
        .with_command("cargo", CommandStance::Clear)
        .build();
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
    use crate::log::{CallId, Spot, Stance};

    // Two `cargo test` invocations, the second carrying a `clear_always` probe vote for `cargo`.
    fn assistant(id: &str, command: &str) -> LogEntry {
        LogEntry::new(
            0,
            Payload::assistant(crate::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![ToolCall::new(
                    BASH,
                    id,
                    serde_json::json!({ "command": command }),
                )],
            )),
        )
    }
    fn spot(id: &str, stance: Stance, probed: Option<&str>) -> LogEntry {
        LogEntry::new(
            0,
            Payload::Spot(
                Spot::new(CallId(id.into()), stance, String::new())
                    .with_probed_prefix(probed.map(str::to_string)),
            ),
        )
    }

    let log = [
        assistant("a", "cargo test"),
        spot("a", Stance::Clear, Some("cargo")), // 1st sighting, not-safe-yet vote? no — clear, not clear_always
        assistant("b", "cargo test"),
        spot("b", Stance::ClearAlways, Some("cargo")), // 2nd sighting + safe vote
    ];
    let rooted_role = ConfigFixture::new().with_path(".", true, true).build();
    let rooted_log = [
        assistant("c", "cd /work/sub && cargo test"),
        spot("c", Stance::Clear, None),
        assistant("d", "cd /work/sub && cargo test"),
        spot("d", Stance::Clear, None),
    ];
    let local = probe_ledger(
        &Directories::at(Path::new("/work")),
        &rooted_role,
        &rooted_log,
    );
    let elsewhere = probe_ledger(
        &Directories::at(Path::new("/elsewhere")),
        &rooted_role,
        &rooted_log,
    );
    assert_eq!(
        local
            .next_probe(&rooted_role, &toks("cargo test"))
            .as_deref(),
        Some("cargo")
    );
    assert_eq!(
        elsewhere.next_probe(&rooted_role, &toks("cargo test")),
        None
    );

    let ledger = probe_ledger(&Directories::at(Path::new(CWD)), &Config::default(), &log);
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
    let log = [LogEntry::new(
        0,
        Payload::Spot(
            Spot::new(
                CallId("x".into()),
                Stance::ClearAlways, // would be a safe vote — but origin is Fallback
                String::new(),
            )
            .with_origin(Origin::Fallback)
            .with_probed_prefix(Some("cargo".into())),
        ),
    )];
    let ledger = probe_ledger(&Directories::at(Path::new(CWD)), &Config::default(), &log);
    assert!(
        !ledger.ready_to_promote("cargo"),
        "a fallback casts no vote"
    );
}
