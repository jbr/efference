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
        handle: "bg#1".into(),
        command: "cargo watch -x test".into(),
        tail: "test result: ok. 12 passed".into(),
        running_secs: 252,
        idle_secs: 8,
    }]));
    play.event(TuiEvent::Usage(UsageStats {
        input_tokens: 12400,
        output_tokens: 3100,
        cache_read_input_tokens: 8900,
        cache_creation_input_tokens: 5200,
        seconds_since_last_request: Some(8),
    }));
    play.snapshot("sidebar_processes_and_stats");
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
        vec![tool_call(
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
        vec![tool_call("bash", "toolu_g", json!({ "command": command }))],
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
        vec![tool_call(
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
        vec![tool_call(
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
/// command, running/idle timing, and the last output line. Fed by the `/state`
/// poll, it is chrome, not transcript (no reproject).
#[test]
fn background_process_panel() {
    use crate::tui::{BgProc, TuiEvent};
    let mut play = Screenplay::new(80, 16);
    play.user("watch the build");
    play.assistant("Started it — I'll keep working.", vec![]);
    play.event(TuiEvent::Processes(vec![
        BgProc {
            handle: "bg#1".into(),
            command: "cargo watch -x test".into(),
            tail: "Compiling efference\nrunning 12 tests\ntest result: ok. 12 passed".into(),
            running_secs: 252,
            idle_secs: 8,
        },
        BgProc {
            handle: "bg#2".into(),
            command: "npm run dev".into(),
            tail: "ready - started server on 0.0.0.0:3000".into(),
            running_secs: 9,
            idle_secs: 9,
        },
    ]));
    play.snapshot("background_process_panel");
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
        vec![tool_call(
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
