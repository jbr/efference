//! What the sidebar displays: processes and stats, folding control characters,
//! retention, the cold-cache-streak warning, session identity, hosted roles,
//! the sidebar-only slice, policy with a tentative proposal, the dock
//! threshold, and the background process panel.

use super::*;

/// The right-docked sidebar (docs/tui.md): on a wide terminal, PROCESSES and STATS
/// render in the right column (no bottom strip), with a `│` divider from the
/// transcript. A glanceable readout — focus + proposals come later.
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
        // Triage retention (docs/redaction-fork.md): 13.2k of 24.1k lines kept →
        // the `kept 55% · 13k/24k lines` line. No gate failed, so nothing withheld.
        kept_lines: 13_276,
        shown_lines: 24_114,
        withheld_results: 0,
        // The cache is being read (`cache_read_input_tokens` above), so no streak.
        cold_streak: 0,
    }));
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
/// (docs/redaction-fork.md).
#[test]
fn sidebar_retention_before_any_triage_and_after_a_failed_gate() {
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
    assert!(after.contains("kept 25% · 30/120 lines"), "{after}");
    assert!(
        after.contains("2 withheld"),
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
    };

    let mut play = Screenplay::new(110, 16);
    play.user("go");
    play.event(TuiEvent::Usage(stats(crate::daemon::COLD_STREAK_WARN - 1)));
    let below = play.sidebar_grid();
    assert!(
        !below.contains("cache cold"),
        "a run of one or two is normal operation, not a warning:\n{below}"
    );

    play.event(TuiEvent::Usage(stats(7)));
    let warned = play.sidebar_grid();
    assert!(
        warned.contains("⚠ cache cold ×7"),
        "the streak is stated beside the ctx meter:\n{warned}"
    );
    assert!(
        warned.contains("ctx 7%"),
        "beside, not instead of:\n{warned}"
    );

    play.event(TuiEvent::Usage(stats(0)));
    assert!(
        !play.sidebar_grid().contains("cache cold"),
        "a warm read clears it"
    );
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
        !before.contains("SESSION"),
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
    assert!(after.contains("SESSION"), "{after}");
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
    assert!(grid.contains("spotter personal/claude-haiku-4-5"), "{grid}");
    assert!(grid.contains("triage shared"), "{grid}");
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
        slice.contains("POLICY"),
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
    assert!(
        widest < 60,
        "the slice is narrow (≈sidebar width), not the full 110 cols; got {widest}"
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

    let bar = play.find("⊕").expect("the proposed grant shows a ⊕ marker");
    assert!(
        bar.0 > 80,
        "the proposal marker is in the right-docked sidebar"
    );
    play.snapshot("sidebar_policy_with_proposal");
}

/// The sidebar docks only at/above the width threshold; below it the layout falls
/// back to no sidebar (the drawer is a later step). Guards the render-side
/// width-responsiveness the resize handler relies on.
#[test]
fn sidebar_docks_only_above_the_width_threshold() {
    let mut narrow = Screenplay::new(80, 10);
    narrow.user("hi");
    assert!(
        narrow.find("PROCESSES").is_none(),
        "no sidebar below the dock threshold (80 cols)"
    );

    let mut wide = Screenplay::new(110, 10);
    wide.user("hi");
    assert!(
        wide.find("PROCESSES").is_some(),
        "the sidebar docks at/above the threshold (110 cols)"
    );
}

/// The live background-process panel (docs/supervisor.md): a `Processes` event renders a
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
