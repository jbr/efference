//! Moving around the sidebar with keyboard and mouse: tab entering on the first
//! actionable row, rows walking processes then proposals, enter firing the
//! focused row's verb, a vanished row no-opping, tab returning to where focus
//! entered from, tab leaving an open unselected sidebar, clicks focusing rows
//! and firing verbs, the dev screenshot matching the snapshot render, view
//! jumping transcript focus, and an unmapped process row offering no view verb.

use super::*;

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
        resolved_review_level: None,
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
    let drawer = play
        .state
        .viewport
        .sidebar_area
        .expect("the drawer is open");
    assert!(bar.0 > drawer.x, "the bar is in the drawer, got {bar:?}");
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
    play.route_key(KeyCode::Right, KeyModifiers::NONE); // → item 1 (sessions)
    play.route_key(KeyCode::Right, KeyModifiers::NONE); // → item 2 (theme)
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
    // Open, the drawer is above the process panel, so its row is found first.
    play.open_drawer();
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

/// The `view` verb (docs/process-monitor.md's call↔process mapping): a process row whose
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
