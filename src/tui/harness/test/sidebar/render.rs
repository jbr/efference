//! What the session drawer displays: processes and stats, folding control
//! characters, retention, the cold-cache-streak warning, session identity, hosted
//! roles, the drawer-only slice, policy with a tentative proposal, opening on
//! demand, and the background process panel.

use super::*;

/// The session drawer (docs/tui.md): PROCESSES and STATS render in the panel over
/// the transcript's right side, with a `│` divider. The process panel above the
/// composer shows the same process whether or not the drawer is open.
#[test]
fn sidebar_shows_processes_and_stats() {
    use crate::tui::{BgProc, TuiEvent, UsageStats};
    let mut play = Screenplay::new(110, 20);
    play.user("Watch the build.");
    play.assistant("Started it — I'll keep working.", vec![]);
    play.event(TuiEvent::Processes(vec![BgProc {
        id: 1,
        command: "cargo watch -x test".into(),
        call_id: None,
        spawned_at: 0,
        last_chunk_at: None,
        tail: "test result: ok. 12 passed".into(),
        running_secs: 252,
        idle_secs: 8,
    }]));
    play.event(TuiEvent::Usage(UsageStats {
        input_tokens: 12400,
        output_tokens: 3100,
        cache_read_input_tokens: 8900,
        cache_creation_input_tokens: 5200,
        last_request_at: None,
        seconds_since_last_request: Some(8),
        // The boot-fetched window and the last request's occupancy
        // (docs/transport.md): 14.1k of 200k → the `ctx 7% · 14k/200k` line.
        context_limit: Some(200_000),
        context_used: Some(14_106),
        // Billed session cost from the provider's accounting → `cost $0.0421`.
        cost_usd: Some(0.0421),
        cost_unpriced: Some(0),
        // Triage retention (docs/triage.md): 13.2k of 24.1k lines kept →
        // the `kept 55% · 13k/24k lines` line. No gate failed, so nothing withheld.
        kept_lines: 13_276,
        shown_lines: 24_114,
        withheld_results: 0,
        // The cache is being read (`cache_read_input_tokens` above), so no streak.
        cold_streak: 0,
        keepalive_note: None,
    }));
    play.open_drawer();
    play.snapshot("sidebar_processes_and_stats");
}

/// A process's command line is whatever the model wrote — a multi-line `python -c`
/// script, say. Newlines go to the sidebar as the replacement glyph rather than into
/// a cell, where ratatui's `cell_width` would assert and kill the TUI thread (and
/// keep killing it on every restart while the process lives in the daemon).
#[test]
fn sidebar_folds_control_characters_in_a_process_command() {
    use crate::tui::{BgProc, TuiEvent};
    let mut play = Screenplay::new(110, 20);
    play.user("Start a producer.");
    play.assistant("Started.", vec![]);
    play.event(TuiEvent::Processes(vec![BgProc {
        id: 5,
        command: "python3 -u -c 'import time\ncount = 0\nwhile True:\n\tcount += 1'".into(),
        call_id: None,
        spawned_at: 0,
        last_chunk_at: None,
        tail: "heartbeat count=1\x1b[0m\r".into(),
        running_secs: 3,
        idle_secs: 0,
    }]));
    let rendered = play.render();
    assert!(rendered.contains("'import time\u{fffd}cou"), "{rendered}");
}

/// Retention is absent, not 0%, before the first result is triaged — and a withheld
/// result is reported as its own condition rather than folded into the ratio
/// (docs/triage.md).
#[test]
fn sidebar_retention_before_any_triage_and_after_a_failed_triage() {
    use crate::tui::{TuiEvent, UsageStats};
    let stats = |kept, shown, withheld| UsageStats {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        last_request_at: None,
        seconds_since_last_request: None,
        context_limit: None,
        context_used: None,
        cost_usd: None,
        cost_unpriced: None,
        kept_lines: kept,
        shown_lines: shown,
        withheld_results: withheld,
        cold_streak: 0,
        keepalive_note: None,
    };

    let mut play = Screenplay::new(110, 16);
    play.user("go");
    play.event(TuiEvent::Usage(stats(0, 0, 0)));
    let before = play.sidebar_grid();
    assert!(
        !before.contains("kept"),
        "nothing triaged yet — an unknown ratio must not render as 0%:\n{before}"
    );

    play.event(TuiEvent::Usage(stats(30, 120, 2)));
    let after = play.sidebar_grid();
    assert!(after.contains("25% · 30/120 lines"), "{after}");
    assert!(
        after
            .lines()
            .any(|l| l.contains("withheld") && l.contains(" 2 ")),
        "a failed gate is its own line, not a lower ratio:\n{after}"
    );
}

/// The cold-cache warning beside the context meter (docs/log-and-projection.md):
/// silent below the threshold, stated once a run of unexplained cold main-loop reads
/// crosses it, and gone as soon as a warm read resets the daemon's count. A moved
/// prefix is otherwise invisible from the client — every turn just costs full price.
#[test]
fn sidebar_warns_on_a_cold_cache_streak() {
    use crate::tui::{TuiEvent, UsageStats};
    let stats = |cold_streak| UsageStats {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        last_request_at: None,
        seconds_since_last_request: None,
        context_limit: Some(200_000),
        context_used: Some(14_106),
        cost_usd: None,
        cost_unpriced: None,
        kept_lines: 0,
        shown_lines: 0,
        withheld_results: 0,
        cold_streak,
        keepalive_note: None,
    };

    let mut play = Screenplay::new(110, 16);
    play.user("go");
    play.event(TuiEvent::Usage(stats(
        crate::protocol::state::COLD_STREAK_WARN - 1,
    )));
    let below = play.sidebar_grid();
    assert!(
        !below.contains("cache misses"),
        "a run of one or two is normal operation, not a warning:\n{below}"
    );

    play.event(TuiEvent::Usage(stats(7)));
    let warned = play.sidebar_grid();
    assert!(
        warned.contains("⚠ cache misses ×7"),
        "the streak is stated beside the ctx meter:\n{warned}"
    );
    assert!(
        warned.contains("7% · 14k/200k"),
        "beside, not instead of:\n{warned}"
    );

    play.event(TuiEvent::Usage(stats(0)));
    assert!(
        !play.sidebar_grid().contains("cache misses"),
        "a warm read clears it"
    );
}

/// The keepalive's stance beside the cache readout (docs/transport.md): stated only
/// while the loop skips its pings or the upstream rejected them, parsed from the
/// `usage.keepalive_skipped` field, and gone once pings resume.
#[test]
fn sidebar_states_the_keepalive_stance() {
    use crate::tui::TuiEvent;
    let usage = |skipped: serde_json::Value| {
        serde_json::json!({
            "cumulative": {
                "input_tokens": 100, "output_tokens": 50,
                "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0,
            },
            "context": {"limit": 200_000, "used": 14_106},
            "keepalive_skipped": skipped,
        })
    };
    let mut play = Screenplay::new(110, 16);
    play.user("go");
    let event = |value| TuiEvent::Usage(crate::tui::parse::parse_usage(&value).unwrap());

    play.event(event(usage(serde_json::Value::Null)));
    assert!(!play.sidebar_grid().contains("keepalive"));

    play.event(event(usage(serde_json::json!({
        "reason": "discard", "at": 1, "output_tokens": 10_000,
        "completion_usd": 0.005, "prefix_tokens": 5_000, "premium_usd": null,
    }))));
    let paused = play.sidebar_grid();
    assert!(
        paused.contains("keepalive paused: 10k out ($0.0050)"),
        "{paused}"
    );

    play.event(event(usage(serde_json::json!({
        "reason": "rejected", "at": 2, "detail": "400",
    }))));
    assert!(play.sidebar_grid().contains("keepalive off: ping rejected"));

    play.event(event(usage(serde_json::Value::Null)));
    assert!(!play.sidebar_grid().contains("keepalive"));
}

/// The SESSION section names the model and the working directory, from the
/// `session_identity` topic (docs/control-api.md). Nothing in the transcript says
/// which model is answering, and cheap and flagship models mix across sessions.
#[test]
fn sidebar_names_the_model_and_working_directory() {
    use crate::tui::{SessionIdentityView, TuiEvent};
    let mut play = Screenplay::new(110, 16);
    play.user("go");
    let before = play.sidebar_grid();
    assert!(
        !before.contains("Session"),
        "no section before the topic arrives:\n{before}"
    );

    play.event(TuiEvent::SessionIdentity(Some(SessionIdentityView {
        id: "amber-heron".into(),
        title: None,
        model: "anthropic/claude-opus-4".into(),
        cwd: "~/code/efference".into(),
        roles: None,
        successor: None,
        root: String::new(),
    })));
    let after = play.sidebar_grid();
    assert!(after.contains("Session"), "{after}");
    assert!(after.contains("amber-heron"), "{after}");
    assert!(after.contains("anthropic/claude-opus-4"), "{after}");
    assert!(after.contains("~/code/efference"), "{after}");

    // A title, once written, leads the section (docs/session-title.md); the
    // topic is re-delivered whole, so the view is replaced rather than patched.
    play.event(TuiEvent::SessionIdentity(Some(SessionIdentityView {
        id: "amber-heron".into(),
        title: Some("Session names: slug ids and titles".into()),
        model: "anthropic/claude-opus-4".into(),
        cwd: "~/code/efference".into(),
        roles: None,
        successor: None,
        root: String::new(),
    })));
    let titled = play.sidebar_grid();
    let title_row = titled
        .lines()
        .position(|l| l.contains("Session names: slug ids and titles"))
        .expect("the title renders");
    let id_row = titled
        .lines()
        .position(|l| l.contains("amber-heron"))
        .expect("the id renders");
    assert!(title_row < id_row, "{titled}");
}

/// A hosted session names each role by connection: the main line becomes the
/// qualified selection, and the spotter and triage rows follow it.
#[test]
fn sidebar_names_hosted_roles_by_connection() {
    use crate::tui::{SessionIdentityView, SessionRolesView, TuiEvent};
    let mut play = Screenplay::new(110, 16);
    play.user("go");
    play.event(TuiEvent::SessionIdentity(Some(SessionIdentityView {
        id: "amber-heron".into(),
        title: None,
        model: "claude-opus-4".into(),
        cwd: "~/code/efference".into(),
        roles: Some(SessionRolesView {
            main: "work/claude-opus-4".into(),
            spotter: "personal/claude-haiku-4-5".into(),
            triage: "shared".into(),
        }),
        successor: None,
        root: String::new(),
    })));
    let grid = play.sidebar_grid();
    assert!(grid.contains("work/claude-opus-4"), "{grid}");
    for (label, value) in [
        ("spotter", "personal/claude-haiku-4-5"),
        ("triage", "shared"),
    ] {
        assert!(
            grid.lines().any(|l| l.contains(label) && l.contains(value)),
            "{label} names {value}:\n{grid}"
        );
    }
}

/// The sidebar-only screenshot slice (`Alt-S`) frames just the sidebar columns:
/// narrow, and excluding the transcript — so a wide-terminal capture isn't verbose.
#[test]
fn sidebar_slice_excludes_the_transcript() {
    use crate::tui::{PolicyEntry, PolicyView, TuiEvent};
    let mut play = Screenplay::new(110, 16);
    play.user("A long transcript line that must NOT appear in a sidebar-only slice.");
    play.assistant("ok", vec![]);
    play.event(TuiEvent::Policy(PolicyView {
        scope: Some("this repo".into()),
        scope_proposal: None,
        commands: vec![PolicyEntry {
            label: "cargo".into(),
            proposed: false,
            seq: None,
        }],
        paths: vec![],
        tools: vec![],
        review_level: Some("human_only".into()),
        resolved_review_level: None,
    }));

    let slice = play.sidebar_grid();
    assert!(
        slice.contains("Policy"),
        "the slice shows the sidebar:\n{slice}"
    );
    assert!(
        slice.contains("human_only"),
        "the sidebar surfaces the review-level dial:\n{slice}"
    );
    assert!(
        !slice.contains("must NOT appear"),
        "the slice excludes the transcript:\n{slice}"
    );
    let widest = slice.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    let drawer = crate::tui::render_frame::DRAWER_WIDTH as usize;
    assert!(
        widest < drawer + 10,
        "the slice is narrow (≈drawer width), not the full 110 cols; got {widest}"
    );
}

/// The POLICY sidebar section: committed grants plus a *proposed* one rendered in
/// its sorted position with a trailing `⊕` (docs/permissions.md) — `git log` among
/// the commands, `/refs` among the paths. Committing would drop the `⊕` in place.
#[test]
fn sidebar_policy_with_a_tentative_proposal() {
    use crate::tui::{PolicyEntry, PolicyView, TuiEvent};
    let committed = |label: &str| PolicyEntry {
        label: label.into(),
        proposed: false,
        seq: None,
    };
    let proposed = |label: &str, seq: u64| PolicyEntry {
        label: label.into(),
        proposed: true,
        seq: Some(seq),
    };
    let mut play = Screenplay::new(110, 28);
    play.user("Work on the repo.");
    play.assistant("On it.", vec![]);
    play.event(TuiEvent::Policy(PolicyView {
        scope: Some("work only in this repo".into()),
        scope_proposal: None,
        commands: vec![
            committed("cargo"),
            committed("git diff"),
            proposed("git log", 7),
        ],
        paths: vec![committed(".  rw"), proposed("/refs  r", 9)],
        tools: vec![committed("read"), committed("write")],
        review_level: None,
        resolved_review_level: None,
    }));

    play.open_drawer();
    let bar = play.find("⊕").expect("the proposed grant shows a ⊕ marker");
    let drawer = play
        .state
        .viewport
        .sidebar_area
        .expect("the drawer is open");
    assert!(bar.0 > drawer.x, "the proposal marker is in the drawer");
    play.snapshot("sidebar_policy_with_proposal");
}

/// The drawer is closed until asked for, at any width: the transcript takes the
/// full width, and the header's **session** button (or Tab from command mode)
/// opens it over the transcript's right side.
#[test]
fn the_drawer_opens_on_demand() {
    for width in [80, 110] {
        let mut play = Screenplay::new(width, 10);
        play.user("hi");
        assert!(
            play.find("Processes").is_none(),
            "closed by default at {width} cols"
        );
        assert!(play.find("session").is_some(), "the header offers it");
        play.open_drawer();
        assert!(
            play.find("Processes").is_some(),
            "open on demand at {width} cols"
        );
    }
}

/// The live background-process panel (docs/process-monitor.md): a `Processes` event renders a
/// persistent strip above the composer — one row per running process with handle,
/// command, running/idle timing, and the last output line. Fed by the `processes`
/// topic, it is chrome, not transcript (no reproject).
#[test]
fn background_process_panel() {
    use crate::tui::{BgProc, TuiEvent};
    let mut play = Screenplay::new(80, 16);
    play.user("watch the build");
    play.assistant("Started it — I'll keep working.", vec![]);
    play.event(TuiEvent::Processes(vec![
        BgProc {
            id: 1,
            command: "cargo watch -x test".into(),
            call_id: None,
            spawned_at: 0,
            last_chunk_at: None,
            tail: "Compiling efference\nrunning 12 tests\ntest result: ok. 12 passed".into(),
            running_secs: 252,
            idle_secs: 8,
        },
        BgProc {
            id: 2,
            command: "npm run dev".into(),
            call_id: None,
            spawned_at: 0,
            last_chunk_at: None,
            tail: "ready - started server on 0.0.0.0:3000".into(),
            running_secs: 9,
            idle_secs: 9,
        },
    ]));
    play.snapshot("background_process_panel");
}

/// The `cold` topic in the header (backlog: cold-session.md): stated while the cache is
/// believed expired, with the compaction key while the `verbs` topic says the compact
/// route takes it, `compacting…` while a compaction is open in `current_operations`,
/// and gone when the topic clears. The park is the `cold_parked` loop phase.
#[test]
fn header_states_an_expired_cache() {
    use crate::protocol::state::LoopPhase;
    use crate::tui::TuiEvent;
    use crate::tui::operations::OperationsView;
    let compacting = |open: bool| {
        TuiEvent::Operations(open.then(|| OperationsView {
            compaction: Some(0),
            ..OperationsView::default()
        }))
    };
    let mut play = Screenplay::new(110, 16);
    play.user("go");
    assert!(!play.render().contains("cache expired"));

    play.event(TuiEvent::CacheExpired(
        Some(crate::tui::ColdView::default()),
    ));
    assert!(play.render().contains("cache expired · C-o compacts"));
    play.event(TuiEvent::Verbs(crate::tui::VerbsView {
        compact_refused: Some("an approval gate is pending — resolve it first".into()),
        ..crate::tui::VerbsView::default()
    }));
    assert!(
        !play.render().contains("C-o compacts"),
        "no key is offered for a compaction the route would refuse"
    );
    play.event(TuiEvent::Verbs(crate::tui::VerbsView::default()));

    play.event(TuiEvent::Phase(LoopPhase::ColdParked));
    assert!(
        play.render()
            .contains("paused: cache expired · ⏎ to continue · C-o compacts")
    );
    play.event(TuiEvent::Phase(LoopPhase::Idle));

    play.event(compacting(true));
    assert!(play.render().contains("cache expired · compacting…"));

    play.event(TuiEvent::CacheExpired(None));
    assert!(!play.render().contains("cache expired"));
    assert!(
        play.render().contains("compacting…"),
        "a warm compaction shows too"
    );

    // A compaction at a park ends the belief, not the park: a send resumes it unasked.
    play.event(compacting(false));
    play.event(TuiEvent::Phase(LoopPhase::ColdParked));
    assert!(play.render().contains("paused: summarized · ⏎ to continue"));
    play.event(TuiEvent::Phase(LoopPhase::Idle));
    assert!(!play.render().contains("compacting…"));
}
