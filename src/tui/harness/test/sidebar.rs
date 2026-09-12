//! The sidebar and the process/gate panels docked beside the transcript.

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

/// A spotter-routed gate renders the spotter's reason (docs/permissions.md) beside the
/// exchange it judged, and offers `allow…` on the strip to add a rule.
#[test]
fn gate_shows_spotter_reason_inline() {
    let mut play = Screenplay::new(80, 14);
    play.user("Clean the build directory.");
    play.assistant(
        "Sure — I'll remove it.",
        vec![ToolCall::new(
            "bash",
            "toolu_g",
            json!({ "command": "rm -rf ./build" }),
        )],
    );
    play.pending_gate_reason(
        "toolu_g",
        "bash",
        json!({ "command": "rm -rf ./build" }),
        Some("deletes outside the git tree — not reversible"),
    );
    let screen = play.render();
    assert!(
        screen.contains("spotter · ") && screen.contains("deletes outside the git tree"),
        "the gate shows the spotter's reason beside the exchange. No stance here: this \
         fixture drives only the live `/state` path, which carries the reason without \
         the `Spot` entry's stance:\n{screen}"
    );
    assert!(
        screen.contains("allow…"),
        "the control strip offers the allow… affordance:\n{screen}"
    );
    assert!(
        !screen.contains(".allow"),
        "the typed .allow form is no longer advertised (the strip is the legend):\n{screen}"
    );
}

/// The whole gate survives a **long** call, because every part of it is a sibling of
/// the exchange rather than a child of it (docs/tui.md). A multi-line bash command
/// fills the exchange's collapsed body on its own; while the spotter's reason and the
/// affordance lived inside that body they were clipped away, and the `⏸` was replaced
/// by the disclosure triangle — so a gate on any realistic command showed no reason, no
/// affordance, and no sign the call was held at all. Asserted without expanding
/// anything: the point is that no interaction is required.
#[test]
fn gate_survives_a_long_command() {
    let command = "set -euo pipefail\n\
                   cd ./build\n\
                   for f in *.o; do\n\
                     echo \"removing $f\"\n\
                     rm -f \"$f\"\n\
                   done\n\
                   rm -rf ./build\n\
                   echo done";
    let mut play = Screenplay::new(80, 24);
    play.user("Clean the build directory.");
    play.assistant(
        "Sure — I'll remove it.",
        vec![ToolCall::new(
            "bash",
            "toolu_g",
            json!({ "command": command }),
        )],
    );
    play.pending_gate_reason(
        "toolu_g",
        "bash",
        json!({ "command": command }),
        Some("deletes outside the git tree — not reversible"),
    );
    let screen = play.render();
    assert!(
        screen.contains("spotter") && screen.contains("deletes outside the git tree"),
        "the reason renders beside the exchange, not inside its collapsed body:\n{screen}"
    );
    assert!(
        screen.contains("awaiting your approval"),
        "the affordance renders beside the exchange too:\n{screen}"
    );
    assert!(
        play.find("⏸").is_some(),
        "the held-call marker rides the affordance, so a collapsible body can't \
         replace it with a disclosure triangle:\n{screen}"
    );
}

/// A **cleared** call carries its verdict too (docs/permissions.md). Nothing ever
/// interrupts for a cleared call, so before this its reason was written to the log and
/// shown to nobody — and cleared calls are most of what the spotter judges. The stance
/// leads the line, so `cleared` and `gated` are told apart at a glance; the whole line
/// stays in the dim register, since after the fact it is a gloss on the call rather than
/// something to act on.
#[test]
fn a_cleared_call_shows_its_verdict() {
    let mut play = Screenplay::new(80, 16);
    play.user("How big is the source tree?");
    play.assistant(
        "Counting it now.",
        vec![ToolCall::new(
            "bash",
            "toolu_c",
            json!({ "command": "wc -l src/*.rs" }),
        )],
    );
    play.spot(
        "toolu_c",
        Stance::Clear,
        "read-only line count inside the repo",
    );
    play.tool_result("toolu_c", "20191 total", false, WireState::Raw);

    let screen = play.render();
    assert!(
        screen.contains("spotter cleared · read-only line count inside the repo"),
        "a cleared call shows its stance and reason:\n{screen}"
    );
    assert!(
        !screen.contains("awaiting"),
        "nothing is asked of the human — the call already ran:\n{screen}"
    );
}

/// A verdict outlives the gate that carried it: once approved, the pending state is gone,
/// but the committed `Spot` still renders — so scrolling back shows *why* a call was held,
/// and (as here) that the human approved something the spotter wanted to stop.
#[test]
fn a_resolved_gate_keeps_its_verdict() {
    let mut play = Screenplay::new(80, 16);
    play.user("Clean the build directory.");
    play.assistant(
        "Sure — I'll remove it.",
        vec![ToolCall::new(
            "bash",
            "toolu_r",
            json!({ "command": "rm -rf ./build" }),
        )],
    );
    play.spot(
        "toolu_r",
        Stance::Gate,
        "deletes outside the git tree — not reversible",
    );
    play.tool_result("toolu_r", "", false, WireState::Raw);

    let screen = play.render();
    assert!(
        screen.contains("spotter gated · deletes outside the git tree"),
        "the verdict survives the gate's resolution:\n{screen}"
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

// ─── Sidebar focus (docs/tui.md) ─────────────────────────────────────────────

use crate::tui::{BgProc, PolicyEntry, PolicyView, SidebarCmd, SidebarRow};

/// Seed a screenplay with one process and one proposed grant each in commands and
/// paths — the minimal actionable sidebar. Returns the screenplay.
fn sidebar_play() -> Screenplay<'static> {
    let mut play = Screenplay::new(110, 24);
    play.user("watch the build");
    play.assistant("On it.", vec![]);
    play.event(TuiEvent::Processes(vec![BgProc {
        id: 3,
        command: "cargo watch -x test".into(),
        call_id: None,
        spawned_at: 0,
        last_chunk_at: None,
        tail: "ok".into(),
        running_secs: 12,
        idle_secs: 3,
    }]));
    play.event(TuiEvent::Policy(PolicyView {
        scope: Some("this repo".into()),
        scope_proposal: None,
        commands: vec![
            PolicyEntry {
                label: "cargo".into(),
                proposed: false,
                seq: None,
            },
            PolicyEntry {
                label: "git log".into(),
                proposed: true,
                seq: Some(7),
            },
        ],
        paths: vec![PolicyEntry {
            label: "/refs  r".into(),
            proposed: true,
            seq: Some(9),
        }],
        tools: vec![],
        review_level: Some("human_only".into()),
    }));
    play
}

/// Tab (from command mode) enters the sidebar on its first actionable row, marked
/// by the `▌` bar in the pad column and the row's verb menu; the composer's Tab is
/// untouched and still types.
#[test]
fn tab_enters_the_sidebar_on_the_first_actionable_row() {
    let mut play = sidebar_play();

    // While composing, Tab is a typing key — it must not move focus. It inserts
    // the four spaces every render path displays a tab as (a raw control char
    // would panic the paint).
    play.route_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(play.state.sidebar_focus, None, "Tab types while composing");
    assert_eq!(
        play.state.composer.text(),
        "    ",
        "Tab inserted four spaces"
    );
    for _ in 0..4 {
        play.route_key(KeyCode::Backspace, KeyModifiers::NONE);
    }

    // `↑` spills to the strip; Tab from there enters the sidebar on the process.
    play.route_key(KeyCode::Up, KeyModifiers::NONE);
    play.route_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        play.state.sidebar_focus,
        Some(SidebarRow::Process(3)),
        "first actionable row is the process"
    );
    let bar = play.find("▌").expect("the focused row shows the focus bar");
    assert!(bar.0 > 60, "the bar is in the sidebar column, got {bar:?}");
    assert!(
        play.find("▸ kill").is_some(),
        "the process row's verb menu renders:\n{}",
        play.render()
    );
    play.snapshot("sidebar_focused_process_row");
}

/// `↑`/`↓` walk the actionable rows in render order — processes, then proposals in
/// their committed sort position — skipping headers, STATS, and committed grants.
#[test]
fn sidebar_rows_walk_processes_then_proposals() {
    let mut play = sidebar_play();
    play.route_key(KeyCode::Up, KeyModifiers::NONE); // → strip
    play.route_key(KeyCode::Tab, KeyModifiers::NONE); // → sidebar, process row
    assert_eq!(play.state.sidebar_focus, Some(SidebarRow::Process(3)));

    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        play.state.sidebar_focus,
        Some(SidebarRow::Proposal(7)),
        "next row is the commands proposal, not a committed grant"
    );
    assert!(
        play.find("▸ commit").is_some(),
        "the proposal's verb menu renders:\n{}",
        play.render()
    );
    play.snapshot("sidebar_focused_proposal_row");

    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        play.state.sidebar_focus,
        Some(SidebarRow::Proposal(9)),
        "then the paths proposal"
    );

    // Movement clamps at both ends — the sidebar is its own column, not an axis stop.
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(play.state.sidebar_focus, Some(SidebarRow::Proposal(9)));
    play.route_key(KeyCode::Up, KeyModifiers::NONE);
    play.route_key(KeyCode::Up, KeyModifiers::NONE);
    play.route_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(play.state.sidebar_focus, Some(SidebarRow::Process(3)));
}

/// `⏎` fires the selected verb by identity: kill on a process row; commit, then
/// (after `→`) cancel, on a proposal row.
#[test]
fn enter_fires_the_focused_rows_verb() {
    let mut play = sidebar_play();
    play.route_key(KeyCode::Up, KeyModifiers::NONE);
    play.route_key(KeyCode::Tab, KeyModifiers::NONE);
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        play.took_sidebar_cmd(),
        Some(SidebarCmd::KillProcess(3)),
        "⏎ on the process row kills it"
    );

    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        play.took_sidebar_cmd(),
        Some(SidebarCmd::CommitProposal(7)),
        "the leading verb of a proposal is commit"
    );

    play.route_key(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        play.find("▸ cancel").is_some(),
        "the selection marker moves to cancel"
    );
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        play.took_sidebar_cmd(),
        Some(SidebarCmd::CancelProposal(7)),
        "→ then ⏎ cancels"
    );
    // `←` clamps at the leading verb.
    play.route_key(KeyCode::Left, KeyModifiers::NONE);
    play.route_key(KeyCode::Left, KeyModifiers::NONE);
    assert!(play.find("▸ commit").is_some(), "selection clamped left");
}

/// A row that vanished between focus landing and `⏎` no-ops with a note instead of
/// firing whatever occupies its position — the identity rule the strip's index
/// focus lacks. The next `↓` starts fresh.
#[test]
fn a_vanished_row_no_ops_instead_of_firing_a_substitute() {
    let mut play = sidebar_play();
    play.route_key(KeyCode::Up, KeyModifiers::NONE);
    play.route_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(play.state.sidebar_focus, Some(SidebarRow::Process(3)));

    // The process exits; the topic re-emits without it.
    play.event(TuiEvent::Processes(vec![]));
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        play.took_sidebar_cmd(),
        None,
        "a stale identity fires nothing"
    );
    assert_eq!(
        play.state.sidebar_focus, None,
        "the vanished row leaves no selection"
    );
    assert!(play.state.sidebar_open, "the column itself stays open");

    // ↓ re-selects from the surviving rows (none selected → the first).
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        play.state.sidebar_focus,
        Some(SidebarRow::Proposal(7)),
        "↓ re-selects from the surviving rows"
    );
}

/// Tab in the sidebar returns to where focus came from — the strip at its item, or
/// the transcript — and Esc homes to the composer from anywhere.
#[test]
fn tab_returns_to_where_focus_entered_from() {
    let mut play = sidebar_play();

    // From the strip: Tab there and back restores the strip item.
    play.route_key(KeyCode::Up, KeyModifiers::NONE); // → strip item 0
    play.route_key(KeyCode::Right, KeyModifiers::NONE); // → item 1
    play.route_key(KeyCode::Tab, KeyModifiers::NONE); // → sidebar
    play.route_key(KeyCode::Tab, KeyModifiers::NONE); // → back
    assert_eq!(
        play.state.strip_focus,
        Some(crate::tui::control_strip::StripVerb::Theme),
        "the strip verb focus was restored, not a position"
    );
    assert_eq!(play.state.sidebar_focus, None);

    // From the transcript: Tab in and back keeps transcript navigation.
    play.route_key(KeyCode::Up, KeyModifiers::NONE); // strip → transcript
    play.route_key(KeyCode::Tab, KeyModifiers::NONE); // → sidebar
    play.route_key(KeyCode::Tab, KeyModifiers::NONE); // → transcript
    assert_eq!(play.state.sidebar_focus, None);
    assert!(!play.state.composing, "still in command mode");
    assert_eq!(play.state.strip_focus, None);

    // Esc homes to the composer from the sidebar (graduated Esc rung 1).
    play.route_key(KeyCode::Tab, KeyModifiers::NONE); // → sidebar
    play.route_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(play.state.composing, "Esc returns to the composer");
    assert_eq!(play.state.sidebar_focus, None);
}

/// Mouse: clicking a row focuses it; clicking a verb selects and fires it.
#[test]
fn clicks_focus_rows_and_fire_verbs() {
    let mut play = sidebar_play();
    let row = play.find("p#3").expect("the process row renders");
    play.click(row.0, row.1);
    assert_eq!(
        play.state.sidebar_focus,
        Some(SidebarRow::Process(3)),
        "the click focused the row"
    );

    // Focus the proposal row by click, then click its commit verb.
    let prop = play.find("git log").expect("the proposal renders");
    play.click(prop.0, prop.1);
    assert_eq!(play.state.sidebar_focus, Some(SidebarRow::Proposal(7)));
    let verb = play.find("commit").expect("the verb menu renders");
    play.click(verb.0 + 1, verb.1);
    assert_eq!(
        play.took_sidebar_cmd(),
        Some(SidebarCmd::CommitProposal(7)),
        "the clicked verb fired"
    );
}

/// The live `Ctrl+S` dev screenshot path must produce byte-identical output to
/// the committed-snapshot `render` path (both go through `frame_grid`). This is
/// what lets a live capture diff directly against a `.snap` — if it ever drifts,
/// a screenshot would silently stop being comparable to the fixtures.
#[test]
fn dev_screenshot_matches_snapshot_render() {
    let mut play = Screenplay::new(80, 24);
    play.user("Show me the project signature.");
    play.assistant(
        "On it.",
        vec![ToolCall::new(
            "bash",
            "toolu_ss",
            json!({ "command": "grep -n 'fn project' src/project.rs" }),
        )],
    );
    play.tool_result(
        "toolu_ss",
        "12:pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {",
        false,
        WireState::Raw,
    );
    assert_eq!(
        play.screenshot_grid(),
        play.render(),
        "the live screenshot path must equal the committed-snapshot render",
    );
}

/// Tab out works from the open-but-unselected state a vanished row leaves behind —
/// the toggle branches on the column being open, not on a row being selected.
#[test]
fn tab_leaves_an_open_unselected_sidebar() {
    let mut play = sidebar_play();
    play.route_key(KeyCode::Up, KeyModifiers::NONE);
    play.route_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(play.state.sidebar_focus, Some(SidebarRow::Process(3)));

    // The process exits; selection drops with it, column stays open.
    play.event(TuiEvent::Processes(vec![]));
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(play.state.sidebar_focus, None);

    // Tab still leaves (back to the strip it came from); a second Tab re-enters.
    play.route_key(KeyCode::Tab, KeyModifiers::NONE);
    assert!(!play.state.sidebar_open, "Tab left the open column");
    assert_eq!(
        play.state.strip_focus,
        Some(crate::tui::control_strip::StripVerb::PaceWidget)
    );
    play.route_key(KeyCode::Tab, KeyModifiers::NONE);
    assert!(play.state.sidebar_open, "Tab re-entered");
}

/// The `view` verb (docs/supervisor.md's call↔process mapping): a process row whose
/// daemon representation carries a `call_id` leads with `view`, and `⏎` there leaves
/// the sidebar and lands transcript focus on that call's entry — no daemon command
/// is sent, because the jump is this client's focus and nothing else.
///
/// `view` leading is deliberate: a bare `⏎` on a process row reads rather than
/// sending SIGKILL.
#[test]
fn view_jumps_transcript_focus_to_the_processs_entry() {
    use crate::tui::state::KeyboardCursor;
    use serde_json::json;

    let mut play = Screenplay::new(110, 24);
    play.user("start the dev server");
    play.assistant(
        "Backgrounding it.",
        vec![ToolCall::new(
            "bash",
            "toolu_bg",
            json!({ "command": "npm run dev", "background": true }),
        )],
    );
    play.tool_result(
        "toolu_bg",
        "backgrounded as p#3.",
        false,
        crate::log::WireState::Raw,
    );
    play.event(TuiEvent::Processes(vec![BgProc {
        id: 3,
        command: "npm run dev".into(),
        call_id: Some("toolu_bg".into()),
        spawned_at: 0,
        last_chunk_at: None,
        tail: "ready".into(),
        running_secs: 12,
        idle_secs: 3,
    }]));

    play.route_key(KeyCode::Up, KeyModifiers::NONE); // → strip
    play.route_key(KeyCode::Tab, KeyModifiers::NONE); // → sidebar, the process row
    assert_eq!(play.state.sidebar_focus, Some(SidebarRow::Process(3)));
    assert!(
        play.find("▸ view").is_some(),
        "view is the leading verb on a mapped process row:\n{}",
        play.render()
    );

    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        play.took_sidebar_cmd(),
        None,
        "view is chrome-only — nothing goes to the daemon"
    );
    assert_eq!(
        play.state.sidebar_focus, None,
        "the jump leaves the sidebar for the transcript"
    );
    assert!(!play.state.composing);
    let KeyboardCursor::Focused { action_index } = play.state.viewport.keyboard_cursor else {
        panic!("view did not land transcript focus");
    };
    assert_eq!(
        play.state.entry_action_index("toolu_bg"),
        Some(action_index),
        "and it landed on the spawning call's own entry"
    );
}

/// A process the daemon reported with no `call_id` — an older daemon, or a process
/// registered outside a tool call — carries no `view` verb at all, rather than one
/// that explains itself away when pressed.
#[test]
fn an_unmapped_process_row_offers_no_view_verb() {
    let mut play = sidebar_play();
    play.route_key(KeyCode::Up, KeyModifiers::NONE);
    play.route_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(play.state.sidebar_focus, Some(SidebarRow::Process(3)));
    assert!(play.find("▸ kill").is_some(), "kill is still there");
    assert!(
        play.find("▸ view").is_none(),
        "no entry to land on → no verb:\n{}",
        play.render()
    );
}
