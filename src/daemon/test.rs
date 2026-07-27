
use super::*;
use crate::client::Client as ControlClient;
use crate::log::ContentBlock;
use futures_lite::StreamExt;
use std::time::Duration;

fn user(text: &str) -> Payload {
    Payload::User(vec![ContentBlock::Text(text.into())])
}

/// `GET /state` reports cache telemetry: the latest `usage`, running totals,
/// and a seconds-since stamp. Drives the cache gate signature through it (docs/README.md) —
/// a cold first request (all creation) then a hot one (reads climb, creation
/// collapses) — and asserts the surface reflects both. Transport-only: the
/// log is untouched, so this never perturbs projection or byte-stability.
#[test]
fn state_reports_cache_telemetry() {
    let log_path = std::env::temp_dir().join("efference-usage-test.jsonl");
    let _ = std::fs::remove_file(&log_path);
    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));

    // No request yet: no `last`, no stamp.
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["usage"]["last"].is_null());
    assert!(v["usage"]["seconds_since_last_request"].is_null());

    // Cold prefix: all creation, no read.
    shared.record_request(Usage {
        input_tokens: 6,
        output_tokens: 10,
        cache_creation_input_tokens: 9407,
        cache_read_input_tokens: 0,
    });
    // Now hot: reads climb, creation collapses (the cache gate signature, docs/README.md).
    shared.record_request(Usage {
        input_tokens: 6,
        output_tokens: 12,
        cache_creation_input_tokens: 165,
        cache_read_input_tokens: 9407,
    });

    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    // `last` is the most recent request.
    assert_eq!(v["usage"]["last"]["cache_read_input_tokens"], 9407);
    assert_eq!(v["usage"]["last"]["cache_creation_input_tokens"], 165);
    // Cumulative sums across both.
    assert_eq!(
        v["usage"]["cumulative"]["cache_creation_input_tokens"],
        9407 + 165
    );
    assert_eq!(v["usage"]["cumulative"]["cache_read_input_tokens"], 9407);
    assert_eq!(v["usage"]["cumulative"]["output_tokens"], 22);
    // A wall-clock stamp now exists — the eviction-prediction signal (docs/log-and-projection.md).
    assert!(v["usage"]["seconds_since_last_request"].is_u64());
}

/// `GET /state` reports the resolved role (docs/permissions.md): a
/// `null` source for the built-in default, and the path plus the **effective** config's full
/// contents (scope, watch-for, commands, tools, paths) when a config is in effect — so "what is
/// my live perimeter?" is answerable, not just "which file?". The contents track the *effective*
/// policy, so a folded config edit shows up (docs/permissions.md).
#[test]
fn state_reports_effective_role_contents() {
    // Default role (no config file) → null source, empty everywhere.
    let (shared, _rx) = Shared::new(Log::in_memory());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert!(v["role"]["source"].is_null());
    assert!(v["role"]["scope"].is_null());
    assert_eq!(v["role"]["commands"], serde_json::json!([]));

    // A loaded config surfaces its resolved path and full contents (sorted, by BTree order).
    let loaded = LoadedConfig {
        role: crate::config::Config {
            scope: Some("stay in the repo".into()),
            watch_for: vec!["network egress".into()],
            commands: ["cargo", "git diff"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            tools: ["read"].iter().map(|s| s.to_string()).collect(),
            paths: [(
                ".".to_string(),
                crate::config::Perms {
                    read: true,
                    write: true,
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
        source: ConfigSource::Path("efference.toml".into()),
    };
    let (shared, _rx) = Shared::with_role(Log::in_memory(), loaded);
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["role"]["source"], "efference.toml");
    assert_eq!(v["role"]["scope"], "stay in the repo");
    assert_eq!(
        v["role"]["watch_for"],
        serde_json::json!(["network egress"])
    );
    assert_eq!(
        v["role"]["commands"],
        serde_json::json!(["cargo", "git diff"])
    );
    assert_eq!(v["role"]["tools"], serde_json::json!(["read"]));
    assert_eq!(
        v["role"]["paths"]["."],
        serde_json::json!({ "read": true, "write": true })
    );

    // The fix: a *committed* config edit moves the reported perimeter (the boot config did not
    // list `npm`; after the edit, `/state` does) — `/state` reflects effective, not base.
    shared.append(0, allow_grant("npm")).unwrap();
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(
        v["role"]["commands"],
        serde_json::json!(["cargo", "git diff", "npm"])
    );
}

/// Autonomous mode defaults off and `/state` reports it both ways — the orchestrator-above
/// reads this to know whether a subordinate runs unsupervised (the autonomous flip).
#[test]
fn state_reports_autonomous_mode() {
    let (shared, _rx) = Shared::new(Log::in_memory());
    assert!(!shared.autonomous());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["autonomous"], false);

    shared.set_autonomous(true);
    assert!(shared.autonomous());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["autonomous"], true);
}

/// The review-level dial (docs/permissions.md) is runtime-settable and
/// reported on `/state`: default `spotter_human`, and `autonomous` is a *derived* mirror of the
/// dial (true iff the level is `Autonomous`) — so the two stay consistent.
#[test]
fn state_reports_the_review_level_dial() {
    use crate::policy::ReviewLevel;
    let (shared, _rx) = Shared::new(Log::in_memory());

    // Default: supervised, not autonomous.
    assert_eq!(shared.review_level(), ReviewLevel::SpotterHuman);
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["review_level"], "spotter_human");
    assert_eq!(v["autonomous"], false);

    // HumanOnly: reported, and still not autonomous (a human is at the gate).
    shared.set_review_level(ReviewLevel::HumanOnly);
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["review_level"], "human_only");
    assert_eq!(v["autonomous"], false);

    // Autonomous: the derived mirror flips true.
    shared.set_review_level(ReviewLevel::Autonomous);
    assert!(shared.autonomous());
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["review_level"], "autonomous");
    assert_eq!(v["autonomous"], true);
}

/// One bash `ToolCall`, for exercising the effective-policy cache through `classify`.
fn bash_call(command: &str) -> ToolCall {
    ToolCall {
        id: CallId("toolu_x".into()),
        name: crate::bash::NAME.into(),
        input: serde_json::json!({ "command": command }),
    }
}

/// A role that resolved from `path`, so `Shared` opens it as the live [`ConfigFile`] — what
/// makes the refresh/revert path reachable in a test without a real daemon boot. The base role
/// stays default: the file's contents reach effective policy through a refresh, like any edit.
fn file_role(path: &std::path::Path) -> LoadedConfig {
    LoadedConfig {
        role: crate::config::Config::default(),
        source: ConfigSource::Path(path.to_path_buf()),
    }
}

fn allow_grant(program: &str) -> Payload {
    committed_edit(crate::log::ConfigChange::Command {
        command: program.into(),
    })
}

/// A committed config edit (the synchronous/direct commit path) as a `Payload` — no prior
/// proposal to resolve.
fn committed_edit(change: crate::log::ConfigChange) -> Payload {
    Payload::ConfigEdit(crate::log::ConfigEdit {
        state: EditState::Committed,
        resolves: None,
        change,
        note: String::new(),
    })
}

/// The effective-policy cache updates incrementally: a `ConfigEdit` appended mid-session
/// promotes a call the base gated into the fast-path, with no log re-scan at the gate
/// (docs/permissions.md).
#[test]
fn appending_a_config_edit_updates_the_effective_config() {
    use crate::policy::{Verdict, classify};
    let call = bash_call("npm install");

    // Empty base role → npm gates.
    let (shared, _rx) = Shared::new(Log::in_memory());
    assert_eq!(classify(&shared.effective_config(), &call), Verdict::Gate);

    // ConfigEdit `allow "npm"` → the same call now clears, via the incrementally-folded role.
    shared.append(0, allow_grant("npm")).unwrap();
    assert_eq!(classify(&shared.effective_config(), &call), Verdict::Clear);
}

/// A session-visible config edit (`scope`, `paths`) also raises a pending notification so the
/// session learns its perimeter moved; a classifier-only edit (`commands`/`tools`) does not
/// (docs/permissions.md).
#[test]
fn session_visible_config_edit_enqueues_a_notification() {
    let edit = committed_edit;

    // A scope edit notifies.
    let (shared, _rx) = Shared::new(Log::in_memory());
    assert!(!shared.has_pending_notifications());
    shared
        .append(
            0,
            edit(crate::log::ConfigChange::Scope {
                text: "stay in src/".into(),
            }),
        )
        .unwrap();
    assert!(
        shared.has_pending_notifications(),
        "scope edit is session-visible"
    );

    // A path edit notifies.
    let (shared, _rx) = Shared::new(Log::in_memory());
    shared
        .append(
            0,
            edit(crate::log::ConfigChange::Path {
                path: ".".into(),
                read: true,
                write: true,
            }),
        )
        .unwrap();
    assert!(
        shared.has_pending_notifications(),
        "path edit is session-visible"
    );

    // A command edit does not — its effect reaches the model only through the classifier.
    let (shared, _rx) = Shared::new(Log::in_memory());
    shared.append(0, allow_grant("npm")).unwrap();
    assert!(
        !shared.has_pending_notifications(),
        "command edit is classifier-only"
    );
}

/// A `Proposed` edit as a `Payload`, for driving the lifecycle in tests.
fn proposed_edit(change: crate::log::ConfigChange) -> Payload {
    Payload::ConfigEdit(crate::log::ConfigEdit {
        state: EditState::Proposed,
        resolves: None,
        change,
        note: String::new(),
    })
}

/// The proposal lifecycle (docs/permissions.md): a `Proposed` edit is
/// **inert** in the effective config but lands in the pending-proposals queue, addressed by its
/// own `seq`; an orchestrator's `Committed` decision (referencing that `seq` via `resolves`)
/// both applies the change *and* drains the proposal from the queue.
#[test]
fn a_proposal_is_inert_until_committed_then_drains_the_queue() {
    use crate::policy::{Verdict, classify};
    let call = bash_call("npm install");
    let (shared, _rx) = Shared::new(Log::in_memory());

    // Propose: inert in policy, but queued.
    let proposal_seq = shared
        .append(
            0,
            proposed_edit(crate::log::ConfigChange::Command {
                command: "npm".into(),
            }),
        )
        .unwrap();
    assert_eq!(
        classify(&shared.effective_config(), &call),
        Verdict::Gate,
        "a proposal must not take effect",
    );
    let queued = shared.pending_proposals();
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].seq, proposal_seq,
        "the proposal is addressed by its own seq"
    );

    // Commit it (referencing the proposal): the change applies and the queue drains.
    shared
        .append(
            0,
            Payload::ConfigEdit(crate::log::ConfigEdit {
                state: EditState::Committed,
                resolves: Some(proposal_seq),
                change: crate::log::ConfigChange::Command {
                    command: "npm".into(),
                },
                note: String::new(),
            }),
        )
        .unwrap();
    assert_eq!(classify(&shared.effective_config(), &call), Verdict::Clear);
    assert!(
        shared.pending_proposals().is_empty(),
        "commit drains the proposal"
    );
}

/// The fs-watch initiator's core (docs/permissions.md): reloading an
/// edited config file emits a `Committed` edit per delta against effective policy, folding it
/// in live; a session-visible delta (`scope`) also notifies; re-running against the *same* file
/// is a no-op echo (the diff is empty). Tests `refresh_and_emit` directly — the synchronous core
/// the notify thread drives — so there's no filesystem-event timing in the test.
#[test]
fn reload_emits_committed_edits_for_file_deltas() {
    use crate::policy::{Verdict, classify};
    let path = std::env::temp_dir().join(format!("efference-watch-{}.toml", std::process::id()));
    std::fs::write(
        &path,
        "scope = \"stay in src/\"\n[commands]\ncargo = true\n",
    )
    .unwrap();

    // The file is opened at construction, but effective policy still starts at the base role —
    // the fold happens on refresh, exactly as it does for an edit arriving mid-session.
    let (shared, _rx) = Shared::with_role(Log::in_memory(), file_role(&path));
    assert_eq!(
        classify(&shared.effective_config(), &bash_call("cargo build")),
        Verdict::Gate
    );

    // First reload folds the file's grants into effective policy and notifies on the scope.
    refresh_and_emit(&shared);
    assert_eq!(
        classify(&shared.effective_config(), &bash_call("cargo build")),
        Verdict::Clear
    );
    assert_eq!(
        shared.effective_config().scope.as_deref(),
        Some("stay in src/")
    );
    assert!(
        shared.has_pending_notifications(),
        "scope delta is session-visible"
    );

    // A second reload against the unchanged file diffs to nothing — the idempotent echo.
    let before = shared.log.lock().unwrap().entries().len();
    refresh_and_emit(&shared);
    assert_eq!(
        shared.log.lock().unwrap().entries().len(),
        before,
        "no-op echo appends nothing"
    );

    std::fs::remove_file(&path).unwrap();
}

/// A config edit that no longer parses keeps the last-good policy, **reverts the file**, and
/// surfaces the parse error to the session — the two-outcome rule
/// (docs/permissions.md). The revert is what makes the file an
/// always-valid artifact: the operator reads it to see the perimeter the session ran under.
#[test]
fn reload_failure_keeps_policy_reverts_file_and_notifies() {
    use crate::policy::{Verdict, classify};
    let path =
        std::env::temp_dir().join(format!("efference-watch-bad-{}.toml", std::process::id()));

    // A good edit first: grants `cargo`, moves policy off the default gate. The comment is
    // here to prove the revert restores bytes, not a re-render of the parsed config.
    let good = "# my config\nscope = \"stay in src/\"\n[commands]\ncargo = true\n";
    std::fs::write(&path, good).unwrap();
    let (shared, _rx) = Shared::with_role(Log::in_memory(), file_role(&path));
    refresh_and_emit(&shared);
    assert_eq!(
        classify(&shared.effective_config(), &bash_call("cargo build")),
        Verdict::Clear
    );

    // Now a malformed save — the exact footgun from session 1784334112726: `[scope]` as a
    // table where the schema wants a string, which fails the whole file.
    std::fs::write(
        &path,
        "[scope]\ndefault = \"read\"\n[commands]\ncargo = true\n",
    )
    .unwrap();
    refresh_and_emit(&shared);

    // Effective policy is unchanged (last good still in force) — fail-closed…
    assert_eq!(
        classify(&shared.effective_config(), &bash_call("cargo build")),
        Verdict::Clear,
        "malformed edit keeps last-good policy"
    );
    // …the file itself is back to the last content that parsed, byte for byte…
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        good,
        "the invalid edit is reverted, comments and all"
    );
    // …but NOT silent: a notice on the parse-error slot tells the session its edit didn't take.
    assert!(
        shared
            .pending_notifications()
            .iter()
            .any(|n| n.slot.as_deref() == Some("config-parse-error")),
        "a parse failure raises a session-visible notice, distinct from the scope-delta notice"
    );

    std::fs::remove_file(&path).unwrap();
}

/// A `Cancelled` decision drains the proposal from the queue and never touches policy.
#[test]
fn cancelling_a_proposal_drains_the_queue_without_applying() {
    use crate::policy::{Verdict, classify};
    let call = bash_call("npm install");
    let (shared, _rx) = Shared::new(Log::in_memory());

    let proposal_seq = shared
        .append(
            0,
            proposed_edit(crate::log::ConfigChange::Command {
                command: "npm".into(),
            }),
        )
        .unwrap();
    assert_eq!(shared.pending_proposals().len(), 1);

    shared
        .append(
            0,
            Payload::ConfigEdit(crate::log::ConfigEdit {
                state: EditState::Cancelled,
                resolves: Some(proposal_seq),
                change: crate::log::ConfigChange::Command {
                    command: "npm".into(),
                },
                note: String::new(),
            }),
        )
        .unwrap();
    assert!(
        shared.pending_proposals().is_empty(),
        "cancel drains the proposal"
    );
    assert_eq!(
        classify(&shared.effective_config(), &call),
        Verdict::Gate,
        "a cancelled proposal never takes effect",
    );
}

/// A gate surfaces the spotter's reason on `/state` (docs/permissions.md): the spotter logs
/// its verdict as a `Spot` *before* routing to the human, so `register_pending` reads the
/// reason from the log — no side channel. The client renders it inline at the gate.
#[test]
fn the_gate_carries_the_spotter_reason() {
    let (shared, _rx) = Shared::new(Log::in_memory());
    let call = bash_call("rm -rf build");
    shared
        .append(
            0,
            Payload::Spot(crate::log::Spot {
                call_id: call.id.clone(),
                stance: crate::log::Stance::Gate,
                reason: "deletes outside the git tree — not reversible".into(),
                origin: crate::log::Origin::Model,
                probed_prefix: None,
            }),
        )
        .unwrap();
    let _gate = shared.register_pending(&call);
    assert!(
        shared
            .state_json()
            .contains("deletes outside the git tree — not reversible"),
        "the gate's /state should carry the spotter's reason",
    );
}

/// The client-facing resolution path (`POST /proposals/:seq` → `resolve_proposal_decision`):
/// commit folds the change and drains the queue; cancel drains without applying; an unknown
/// `seq` resolves nothing.
#[test]
fn resolve_proposal_decision_commits_and_cancels() {
    use crate::policy::{Verdict, classify};
    let (shared, _rx) = Shared::new(Log::in_memory());

    assert!(
        !shared
            .resolve_proposal_decision(999, true, String::new())
            .unwrap(),
        "an unknown seq resolves nothing",
    );

    // Commit: a proposed npm grant takes effect and drains.
    let npm = bash_call("npm install");
    let seq = shared
        .append(
            0,
            proposed_edit(crate::log::ConfigChange::Command {
                command: "npm".into(),
            }),
        )
        .unwrap();
    assert_eq!(classify(&shared.effective_config(), &npm), Verdict::Gate);
    assert!(
        shared
            .resolve_proposal_decision(seq, true, "looks safe".into())
            .unwrap()
    );
    assert_eq!(
        classify(&shared.effective_config(), &npm),
        Verdict::Clear,
        "commit folds the proposed change",
    );
    assert!(
        shared.pending_proposals().is_empty(),
        "commit drains the queue"
    );

    // Cancel: a proposed cargo grant is dropped without taking effect.
    let cargo = bash_call("cargo build");
    let seq2 = shared
        .append(
            0,
            proposed_edit(crate::log::ConfigChange::Command {
                command: "cargo".into(),
            }),
        )
        .unwrap();
    assert!(
        shared
            .resolve_proposal_decision(seq2, false, "not now".into())
            .unwrap()
    );
    assert_eq!(
        classify(&shared.effective_config(), &cargo),
        Verdict::Gate,
        "cancel does not apply the change",
    );
    assert!(
        shared.pending_proposals().is_empty(),
        "cancel drains the queue"
    );
}

/// The `/settle` predicate (docs/permissions.md) tracks the loop's
/// lifecycle: idle → busy on submit (even before dequeue) → busy mid-turn → *settled* at a gate
/// → busy after the gate resolves → settled again when the loop parks.
#[test]
fn settle_predicate_tracks_the_loop_lifecycle() {
    let (shared, _rx) = Shared::new(Log::in_memory());
    assert!(shared.is_settled(), "a fresh daemon is idle");

    // Submitted but not yet dequeued — busy (the submit-race guard via the queued count).
    shared.submit_turn("hi".into());
    assert!(
        !shared.is_settled(),
        "a queued turn is busy before the loop sees it"
    );

    // Dequeued, running mid-turn — still busy.
    shared.dequeued_turn();
    assert!(!shared.is_settled(), "mid-turn is busy");

    // A gate is a decision point — settled.
    let call = bash_call("npm install");
    let _gate = shared.register_pending(&call);
    assert!(
        shared.is_settled(),
        "a pending gate is a settled decision point"
    );

    // Resolving the gate while the turn continues — busy again.
    shared.resolve_approval(&call.id, Approval::Approve);
    assert!(!shared.is_settled(), "post-gate, mid-turn is busy");

    // The loop parks — idle, settled.
    shared.parking();
    assert!(
        shared.is_settled(),
        "a parked loop with nothing queued is settled"
    );
}

/// `await_settled` blocks while the loop is busy and returns the moment it parks — and the
/// `enable()`-before-check ordering means a `parking()` racing the registration isn't lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn await_settled_returns_when_the_loop_parks() {
    let (shared, _rx) = Shared::new(Log::in_memory());
    shared.submit_turn("hi".into());
    shared.dequeued_turn(); // busy, mid-turn

    let waiter = {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move { shared.await_settled().await })
    };
    // Give the waiter a chance to register and confirm it's still blocked on a busy loop.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !waiter.is_finished(),
        "await_settled must block while the loop is busy"
    );

    // Parking settles it; the waiter wakes and returns.
    shared.parking();
    tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("await_settled should return promptly after parking")
        .expect("waiter task panicked");
}

/// A `config_edit` decision body decodes into its change, the wire shape the orchestrator
/// posts to upgrade a gate (docs/permissions.md). A `lifetime` from an older
/// client is ignored rather than rejected — every committed change is written to the file now,
/// so there is no lifetime left to choose.
#[test]
fn config_edit_decision_body_decodes() {
    let body = r#"{"decision":"config_edit","lifetime":"persist","note":"in scope",
                       "change":{"command":{"command":"cargo nextest"}}}"#;
    let d: DecisionBody = serde_json::from_str(body).unwrap();
    assert_eq!(d.decision, "config_edit");
    assert_eq!(
        d.change,
        Some(crate::log::ConfigChange::Command {
            command: "cargo nextest".into()
        })
    );
}

/// On resume, grants already in the log fold into the *initial* effective role at
/// construction — the policy survives a restart even before any new grant lands.
#[test]
fn resumed_log_edits_fold_into_the_initial_effective_config() {
    use crate::policy::{Verdict, classify};
    let mut log = Log::in_memory();
    log.append(0, allow_grant("npm")).unwrap();

    let (shared, _rx) = Shared::new(log);
    assert_eq!(
        classify(&shared.effective_config(), &bash_call("npm install")),
        Verdict::Clear
    );
}

/// The keepalive's load-bearing invariant (docs/log-and-projection.md): a ping advances the
/// cache-touch clock (it really did refresh the TTL) but must NOT reset the
/// real-turn clock — that clock gates the give-up cap, and a ping resetting its
/// own deadline would pin the cache forever. So after a real turn then a ping,
/// the cap clock has been ticking strictly longer than the touch clock.
#[test]
fn keepalive_advances_touch_clock_not_cap_clock() {
    let log_path = std::env::temp_dir().join("efference-keepalive-test.jsonl");
    let _ = std::fs::remove_file(&log_path);
    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));

    // Nothing to keep warm yet → both clocks absent.
    assert_eq!(shared.keepalive_clocks(), (None, None));

    let usage = Usage {
        input_tokens: 1,
        output_tokens: 1,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 100,
    };
    shared.record_request(usage.clone()); // real turn → stamps both clocks
    std::thread::sleep(Duration::from_millis(5));
    shared.record_keepalive(usage); // ping → advances the touch clock only

    let (touch, turn) = shared.keepalive_clocks();
    let (touch, turn) = (touch.unwrap(), turn.unwrap());
    assert!(
        turn > touch,
        "cap clock {turn:?} must outrun the touch clock {touch:?} a ping reset"
    );
}

/// (id, event_type, data) of the next SSE event, with a timeout so a hung tail
/// fails loudly instead of blocking the suite.
async fn next_event(
    events: &mut trillium_client::sse::EventStream,
) -> (Option<String>, Option<String>, String) {
    let event = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("an event within the timeout")
        .expect("the stream has not ended")
        .expect("the event parsed");
    (
        event.id().map(str::to_string),
        event.event_type().map(str::to_string),
        event.data().to_string(),
    )
}

/// `GET /events` replays the pre-existing log (id == seq) and then follows new
/// appends live — including an append that lands *after* the client has caught
/// up and is awaiting, which is the lost-wakeup path the tail is built to
/// survive (the `Notified` is armed before the catch-up read).
///
/// Multi-thread flavor: an SSE body streams server-write ↔ client-read
/// concurrently, which a single-threaded test runtime can deadlock (the real
/// daemon runs on the multi-thread runtime).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_replays_then_tails_live() {
    let dir = std::env::temp_dir();
    let log_path = dir.join("efference-sse-test.jsonl");
    let socket = dir.join("efference-sse-test.sock");
    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&socket);

    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
    // Two entries present before any client connects (the replay set).
    shared.append(1, user("first")).unwrap();
    shared.append(2, user("second")).unwrap();

    trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .expect("bind uds")
        .spawn(router());

    let client = ControlClient::new(&socket);
    let mut events = client.events(Some(0)).await.expect("open events");

    // Replay: the two pre-existing entries, in order, id == seq, typed `user`.
    let (id, kind, data) = next_event(&mut events).await;
    assert_eq!(id.as_deref(), Some("0"));
    assert_eq!(kind.as_deref(), Some("user"));
    assert!(data.contains("first"));
    let (id, _, data) = next_event(&mut events).await;
    assert_eq!(id.as_deref(), Some("1"));
    assert!(data.contains("second"));

    // Live tail: append once the client is caught up and parked on the wait.
    shared.append(3, user("third")).unwrap();
    let (id, _, data) = next_event(&mut events).await;
    assert_eq!(id.as_deref(), Some("2"));
    assert!(data.contains("third"));

    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&log_path);
}

/// `GET /stream` carries ephemeral deltas published after a client subscribes:
/// assistant prose (`assistant_delta`) and tool output (`tool_output`), each
/// typed, JSON-bodied, and crucially **id-less** — they're pre-commit and have
/// no `seq`, which is the contract that keeps them off the durable `/events`
/// cursor. The handler subscribes synchronously while building the response, so
/// a publish after `stream()` resolves can't race ahead of the subscription.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_carries_ephemeral_deltas() {
    let dir = std::env::temp_dir();
    let log_path = dir.join("efference-stream-test.jsonl");
    let socket = dir.join("efference-stream-test.sock");
    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&socket);

    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
    trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .expect("bind uds")
        .spawn(router());

    let client = ControlClient::new(&socket);
    let mut stream = client.stream().await.expect("open stream");

    // Published only after the subscription exists (the handler subscribed
    // before returning the response headers `stream()` just awaited).
    shared.publish_delta(StreamDelta::AssistantText("hello ".into()));
    shared.publish_delta(StreamDelta::ToolOutput {
        call_id: "toolu_5".into(),
        chunk: "line of output\n".into(),
    });
    shared.publish_delta(StreamDelta::BgOutput {
        handle: "bg#1".into(),
        chunk: "tick 3\n".into(),
    });

    // Prose delta: typed, id-less, text in a JSON body.
    let (id, kind, data) = next_event(&mut stream).await;
    assert_eq!(id, None, "deltas carry no seq");
    assert_eq!(kind.as_deref(), Some("assistant_delta"));
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["text"], "hello ");

    // Tool-output delta: tagged by the call id so a client can attribute it.
    let (id, kind, data) = next_event(&mut stream).await;
    assert_eq!(id, None);
    assert_eq!(kind.as_deref(), Some("tool_output"));
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["call_id"], "toolu_5");
    assert_eq!(v["chunk"], "line of output\n");

    // Background-process delta: tagged by handle (TUI-bg-2), demuxed like a tool.
    let (id, kind, data) = next_event(&mut stream).await;
    assert_eq!(id, None);
    assert_eq!(kind.as_deref(), Some("bg_output"));
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["handle"], "bg#1");
    assert_eq!(v["chunk"], "tick 3\n");

    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&log_path);
}

/// The inbox supersedes by slot (archive-and-replace, never edit — docs/inbox.md, docs/log-and-projection.md) and
/// dismisses by id; a superseded or dismissed entry is gone from the pending set.
#[test]
fn inbox_supersedes_by_slot_and_dismisses() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let _a = shared.notify(
        "test-watch".into(),
        "running 5min".into(),
        Some("bash#7".into()),
        None,
    );
    let b = shared.notify("reviewer".into(), "nit at L40".into(), None, None);
    assert_eq!(shared.pending_notifications().len(), 2);

    // Same slot → replace, not append (the "running 5min" → "finished" case).
    let _c = shared.notify(
        "test-watch".into(),
        "finished".into(),
        Some("bash#7".into()),
        None,
    );
    let pending = shared.pending_notifications();
    assert_eq!(pending.len(), 2, "slot supersession replaced, didn't add");
    assert!(pending.iter().any(|n| n.body == "finished"));
    assert!(
        !pending.iter().any(|n| n.body == "running 5min"),
        "superseded entry gone"
    );

    // Explicit dismiss by id; a second dismiss of the same id is a no-op.
    assert!(shared.dismiss_notification(b));
    assert!(!shared.dismiss_notification(b));
    let pending = shared.pending_notifications();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].body, "finished");
}

/// A backgrounded command runs end-to-end through the supervisor (docs/supervisor.md): it
/// spawns, its output drains into the registry, and on exit it fires an `Ambient`
/// exit notification into the inbox — status + final tail — where a triage gate
/// would adjudicate it. Exercises the real spawn/drain/finish/notify path with no
/// model in the loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_process_drains_and_notifies_on_exit() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let handle = shared
        .spawn_background("echo hello; echo world 1>&2".into())
        .expect("spawn");
    assert_eq!(handle, "bg#1");

    // The detached drain task runs on the runtime; poll until the exit
    // notification lands (the command exits near-immediately).
    let mut tries = 0;
    loop {
        let pending = shared.pending_notifications();
        if let Some(n) = pending.iter().find(|n| n.source == "bg#1") {
            assert!(
                n.body.contains("exited 0"),
                "exit status in body: {}",
                n.body
            );
            assert!(
                n.body.contains("hello") || n.body.contains("world"),
                "final tail carried: {}",
                n.body
            );
            assert_eq!(n.slot.as_deref(), Some("bg#1"), "exit rides the bg slot");
            break;
        }
        tries += 1;
        assert!(tries < 100, "exit notification never arrived: {pending:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Exited → no longer in the ambient show or the teardown set.
    assert!(
        shared.bg_snapshot().is_empty(),
        "exited process off the peek"
    );
    shared.kill_all_bg(); // no-op (nothing running) — must not panic.
}

/// The foreground→background handoff end to end (docs/supervisor.md): a foreground
/// bash command is detached mid-run, adopted into the supervisor with its
/// already-streamed bytes as the seed, the continuing drain lands the
/// post-detach output in the same capture, and the exit notifies on the
/// `bg#` slot like any spawned background process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopt_background_continues_the_drain_and_notifies_on_exit() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    let detach = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Interrupt::Detach
    };
    let outcome = crate::bash::Bash::default()
        .run(
            &serde_json::json!({ "command": "echo early; sleep 0.6; echo late" }),
            detach,
            |_| {},
        )
        .await;
    let detached = match outcome {
        crate::bash::Outcome::Detached(d) => d,
        crate::bash::Outcome::Done(r) => panic!("expected a detach, got {:?}", r.content),
    };
    let handle = shared.adopt_background(*detached);
    assert_eq!(handle, "bg#1");

    // The seeded capture is readable immediately, before any new output.
    let read = shared.bg_read_since(&handle, None).expect("known handle");
    assert!(read.contains("early"), "seed in the capture: {read}");

    // The continuing drain appends the post-detach output to the same
    // capture, and the exit notifies — one uninterrupted stream.
    let mut tries = 0;
    loop {
        let pending = shared.pending_notifications();
        if let Some(n) = pending.iter().find(|n| n.source == "bg#1") {
            assert!(n.body.contains("exited 0"), "clean exit: {}", n.body);
            assert!(
                n.body.contains("early") && n.body.contains("late"),
                "final tail spans the whole run: {}",
                n.body
            );
            break;
        }
        tries += 1;
        assert!(tries < 100, "exit notification never arrived: {pending:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A background spawn exports the session scratch dir as `$TMPDIR`, mirroring
/// the foreground bash spawn (docs/tools.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_spawn_exports_the_scratch_tmpdir() {
    let (shared, _turns_rx) = Shared::with_role_and_scratch(
        Log::in_memory(),
        LoadedConfig::default(),
        Some(PathBuf::from("/scratch/efference-test")),
    );
    let handle = shared
        .spawn_background("echo $TMPDIR".into())
        .expect("spawn");
    let mut tries = 0;
    loop {
        let read = shared.bg_read_since(&handle, None).expect("known handle");
        if read.contains("/scratch/efference-test") {
            break;
        }
        tries += 1;
        assert!(tries < 100, "TMPDIR never appeared in the capture: {read}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The detach note (docs/supervisor.md): stashed by `POST /background`,
/// consumed exactly once by the loop when a handoff lands; a blank note is
/// dropped at the door.
#[test]
fn detach_note_is_stashed_and_consumed_once() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    shared.request_detach(Some("check the logs".into()));
    assert_eq!(shared.take_detach_note().as_deref(), Some("check the logs"));
    assert_eq!(shared.take_detach_note(), None, "consumed once");
    shared.request_detach(Some("   ".into()));
    assert_eq!(shared.take_detach_note(), None, "blank note dropped");
}

/// `GET /state` surfaces the live background processes (docs/supervisor.md) — the data the
/// TUI process panel polls. A running child appears with its handle/command;
/// `bg_snapshot`/`state_json` read the registry synchronously after spawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_json_surfaces_running_processes() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    shared.spawn_background("sleep 5".into()).expect("spawn");
    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    let procs = v["processes"]
        .as_array()
        .expect("processes array on /state");
    assert_eq!(procs.len(), 1);
    assert_eq!(procs[0]["handle"], "bg#1");
    assert_eq!(procs[0]["command"], "sleep 5");
    assert!(
        procs[0]["running_secs"].is_u64(),
        "timing present: {procs:?}"
    );
    shared.kill_all_bg();
}

/// `inbox_wake` (docs/inbox.md — the idle wake signal) resolves immediately when a
/// notification is already pending, and otherwise unblocks when one arrives. Drained
/// (shown) notifications don't re-trigger it, so a deferred item can't spin the loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbox_wake_fires_on_pending_not_on_drained() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());

    // Already pending → resolves at once.
    shared.notify("ci".into(), "failed".into(), None, None);
    tokio::time::timeout(Duration::from_secs(2), shared.inbox_wake())
        .await
        .expect("woke on an existing pending notification");

    // Drain it (now "shown") → the queue is empty, so the wake blocks…
    let _ = shared.drain_inbox();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), shared.inbox_wake())
            .await
            .is_err(),
        "a drained notification must not keep waking the idle loop"
    );

    // …until a fresh one arrives.
    let s2 = Arc::clone(&shared);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        s2.notify("op".into(), "nudge".into(), None, None);
    });
    tokio::time::timeout(Duration::from_secs(2), shared.inbox_wake())
        .await
        .expect("woke on a freshly-arrived notification");
}

/// The directed Slice-2 reads/acts (docs/supervisor.md, docs/inbox.md): `kill_bg` finds a running child
/// (and rejects unknown handles), and `inbox_summary` lists pending notifications.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_bg_and_inbox_summary() {
    let (shared, _turns_rx) = Shared::new(Log::in_memory());
    assert!(!shared.kill_bg("bg#1"), "unknown handle → false");

    let h = shared.spawn_background("sleep 30".into()).expect("spawn");
    assert!(shared.kill_bg(&h), "kills a running process");

    // Inbox read: empty, then surfaces a posted notification.
    assert!(
        shared.inbox_summary().contains("empty"),
        "{}",
        shared.inbox_summary()
    );
    shared.notify("ci".into(), "build failed".into(), None, None);
    let summary = shared.inbox_summary();
    assert!(
        summary.contains("ci") && summary.contains("build failed"),
        "{summary}"
    );

    shared.kill_all_bg();
}

/// `POST /notify`, slot supersession, `GET /state` exposure, and
/// `POST /notifications/:id/dismiss` over the real router + control client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_supersede_and_dismiss_over_the_socket() {
    let dir = std::env::temp_dir();
    let log_path = dir.join("efference-notify-test.jsonl");
    let socket = dir.join("efference-notify-test.sock");
    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&socket);

    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
    trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .expect("bind uds")
        .spawn(router());

    let client = ControlClient::new(&socket);
    let gone = client
        .notify("test-watch", "running 5min", Some("bash#7"), None)
        .await
        .expect("notify");
    // Same slot supersedes the first.
    client
        .notify("test-watch", "finished, 0 failures", Some("bash#7"), None)
        .await
        .expect("notify 2");

    let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
    let notes = state["pending_notifications"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "slot superseded, not appended: {notes:?}");
    assert_eq!(notes[0]["body"], "finished, 0 failures");
    assert_eq!(notes[0]["source"], "test-watch");
    let live_id = notes[0]["id"].as_u64().unwrap();

    // Dismissing the live one empties the inbox; dismissing the superseded id 404s.
    client.dismiss_notification(live_id).await.unwrap();
    let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
    assert!(
        state["pending_notifications"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        client.dismiss_notification(gone).await.is_err(),
        "the superseded id is gone → 404"
    );

    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&log_path);
}

/// `POST /review-level` over the real router + control client (docs/permissions.md):
/// a valid level is set and echoed on `/state`; an unknown level is rejected (400) and the dial
/// is left untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_level_over_the_socket() {
    let dir = std::env::temp_dir();
    let log_path = dir.join("efference-review-level-test.jsonl");
    let socket = dir.join("efference-review-level-test.sock");
    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&socket);

    let (shared, _turns_rx) = Shared::new(Log::create(&log_path).expect("create log"));
    trillium_tokio::config()
        .with_shared_state(Arc::clone(&shared))
        .listeners()
        .bind_uds(&socket)
        .expect("bind uds")
        .spawn(router());

    let client = ControlClient::new(&socket);
    assert_eq!(
        client.review_level("human_only").await.unwrap(),
        "human_only"
    );
    let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
    assert_eq!(state["review_level"], "human_only");
    assert_eq!(state["autonomous"], false, "human_only is not autonomous");

    // An unknown level is rejected and does not move the dial.
    assert!(client.review_level("wat").await.is_err(), "unknown → error");
    let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
    assert_eq!(state["review_level"], "human_only", "unchanged after a 400");

    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&log_path);
}

/// The seam-drain (docs/control-channel.md): turns submitted while the loop
/// is mid-trajectory sit in the turn inbox until the loop drains them at a juncture. The
/// non-blocking drain must (a) return every non-blank queued turn in submission order,
/// (b) drop blank turns, and (c) balance the `queued` settle-counter for *every* drained
/// turn — so once the trajectory ends `/settle` can settle instead of reading "busy"
/// forever. Without the counter balance the last assertion fails.
#[test]
fn drain_injected_turns_delivers_queued_turns_and_balances_settle() {
    let (shared, turns_rx) = Shared::new(Log::in_memory());
    let mut frontend = RemoteFrontend {
        shared: Arc::clone(&shared),
        turns: turns_rx,
    };

    // Three turns arrive while the loop is busy (mid-trajectory), one of them blank.
    shared.submit_turn("first".into());
    shared.submit_turn("   ".into());
    shared.submit_turn("second".into());
    assert!(
        !shared.is_settled(),
        "queued turns → not settled while they wait"
    );

    let drained = frontend.drain_injected_turns();
    assert_eq!(
        drained,
        vec!["first".to_string(), "second".to_string()],
        "non-blank turns in submission order; the blank one is dropped"
    );
    assert!(
        shared.is_settled(),
        "every drained turn (blank included) balanced the queued counter, so the loop can settle"
    );

    // A second drain with nothing queued is an empty, no-op pull.
    assert!(frontend.drain_injected_turns().is_empty());
}

/// Pace (docs/control-channel.md) is operator-view state: `set_pace` moves the
/// speed dial and the hold overlay independently, `held` is an overlay that retains `speed`
/// underneath (resume returns to the retained speed, never zero), and `/state` reports both.
#[test]
fn set_pace_moves_the_dials_and_retains_speed_under_a_hold() {
    let (shared, _rx) = Shared::new(Log::in_memory());
    // Default: fast, not held.
    assert_eq!(shared.pace_snapshot(), (Duration::ZERO, false));

    let (speed, held) = shared.set_pace(Some(Duration::from_secs(10)), Some(true));
    assert_eq!((speed, held), (Duration::from_secs(10), true));

    // Releasing keeps the retained speed — the overlay drops, the dial does not zero.
    let (speed, held) = shared.set_pace(None, Some(false));
    assert_eq!((speed, held), (Duration::from_secs(10), false));

    let v: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(v["pace"]["speed_ms"], 10_000);
    assert_eq!(v["pace"]["held"], false);
}

/// Default pace never blocks the seam (fast path: no hold, zero speed).
#[tokio::test]
async fn pace_at_seam_is_a_noop_by_default() {
    let (shared, _rx) = Shared::new(Log::in_memory());
    tokio::time::timeout(Duration::from_millis(200), shared.pace_at_seam())
        .await
        .expect("default pace does not block the seam");
}

/// A hold blocks the seam indefinitely; a **release** ends it immediately (the resume trigger).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hold_blocks_the_seam_until_released() {
    let (shared, _rx) = Shared::new(Log::in_memory());
    shared.set_pace(None, Some(true)); // hold

    // While held, the seam does not return.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), shared.pace_at_seam())
            .await
            .is_err(),
        "a held seam blocks"
    );

    // A release from another task wakes the waiting seam.
    let s2 = Arc::clone(&shared);
    let seam = tokio::spawn(async move { s2.pace_at_seam().await });
    tokio::time::sleep(Duration::from_millis(20)).await; // let the seam arm + await
    shared.set_pace(None, Some(false)); // release
    tokio::time::timeout(Duration::from_millis(500), seam)
        .await
        .expect("release wakes the held seam")
        .expect("seam task joined");
}

/// Submit ⇒ resume, always: a submitted turn drops the hold, and a queued turn skips the seam
/// wait outright (never pace over pending input).
#[tokio::test]
async fn a_submit_resumes_and_skips_the_seam() {
    let (shared, _rx) = Shared::new(Log::in_memory());
    shared.set_pace(None, Some(true));
    assert!(shared.pace_snapshot().1, "held before submit");

    shared.submit_turn("go".into());
    assert!(!shared.pace_snapshot().1, "submit dropped the hold");

    // A turn is queued, so the seam returns at once even though we had been held.
    tokio::time::timeout(Duration::from_millis(200), shared.pace_at_seam())
        .await
        .expect("a queued turn ends the seam wait immediately");
}

/// A finite paced sleep is interruptible: a submitted message ends the delay early rather than
/// making the operator's steer wait out the remaining pace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_submit_interrupts_a_finite_paced_sleep() {
    let (shared, _rx) = Shared::new(Log::in_memory());
    shared.set_pace(Some(Duration::from_secs(30)), None); // slow, not held

    let s2 = Arc::clone(&shared);
    let seam = tokio::spawn(async move { s2.pace_at_seam().await });
    tokio::time::sleep(Duration::from_millis(20)).await; // let the seam enter its sleep
    shared.submit_turn("urgent".into()); // fires the pace interrupt

    tokio::time::timeout(Duration::from_millis(500), seam)
        .await
        .expect("submit ends the 30s paced sleep early")
        .expect("seam task joined");
}

/// `POST /interrupt` **latches** (docs/control-channel.md). The `Notify` half wakes only a
/// currently-registered waiter and stores no permit, so during any tool that doesn't await the
/// abort future — every non-bash backend — the wakeup lands nowhere. The flag is what the loop
/// reads at its junctures instead, and it is a one-shot: consumed exactly once, so a single
/// press stops one call and not the turn after it.
#[test]
fn an_interrupt_latches_and_is_consumed_exactly_once() {
    let (shared, _rx) = Shared::new(Log::in_memory());
    assert!(!shared.take_interrupt(), "nothing latched at rest");

    shared.interrupt(); // no waiter registered — the notify half reaches nobody
    assert!(
        shared.take_interrupt(),
        "the flag survived the dropped wakeup"
    );
    assert!(!shared.take_interrupt(), "one press, one stop");

    // Repeated presses before anyone reads still amount to one interrupt.
    shared.interrupt();
    shared.interrupt();
    assert!(shared.take_interrupt());
    assert!(!shared.take_interrupt());
}
