//! The gate panel: the spotter's reason shown inline, surviving a long command,
//! a cleared call showing its verdict, and a resolved gate keeping its verdict.

use super::*;

/// A spotter-routed gate renders the spotter's reason (docs/permissions.md) beside the
/// exchange it judged; the strip offers `approve` and `abort` and no grant verb.
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
        screen.contains("approve") && screen.contains("abort"),
        "the control strip offers approve and abort:\n{screen}"
    );
    assert!(
        !screen.contains("allow"),
        "no grant verb or typed form is offered at a gate:\n{screen}"
    );
}

/// A gate that reached the operator by escalation names who sent it and shows the note
/// they wrote (docs/permissions.md). The note takes the spotter's place: it is what the
/// escalator wants the decider to know now, where the spotter's account is the older
/// judgment on the call.
#[test]
fn an_escalated_gate_shows_its_note() {
    let mut play = Screenplay::new(80, 16);
    play.user("Clean the build directory.");
    play.assistant(
        "Sure — I'll remove it.",
        vec![ToolCall::new(
            "bash",
            "toolu_e",
            json!({ "command": "rm -rf ./build" }),
        )],
    );
    play.pending_gate_escalated(
        "toolu_e",
        "bash",
        json!({ "command": "rm -rf ./build" }),
        Some("deletes outside the git tree — not reversible"),
        Some("session[calm-finch]"),
        Some("Please confirm: this changes the shared build."),
    );
    let screen = play.render();
    assert!(
        screen.contains("escalated by calm-finch")
            && screen.contains("Please confirm: this changes the shared build."),
        "the gate names its escalator and shows the note:\n{screen}"
    );
    assert!(
        !screen.contains("deletes outside the git tree"),
        "a note replaces the spotter's account rather than stacking under it:\n{screen}"
    );
    assert!(
        screen.contains("awaiting your approval"),
        "the affordance still renders — the escalation does not take the decision away:\n{screen}"
    );
}

/// An escalation that said nothing keeps the spotter's account, attributed: the operator
/// still sees why the call was judged dangerous, and that someone sent it on without a
/// word (docs/permissions.md).
#[test]
fn a_noteless_escalation_keeps_the_spotter_reason() {
    let mut play = Screenplay::new(80, 16);
    play.user("Clean the build directory.");
    play.assistant(
        "Sure — I'll remove it.",
        vec![ToolCall::new(
            "bash",
            "toolu_n",
            json!({ "command": "rm -rf ./build" }),
        )],
    );
    play.pending_gate_escalated(
        "toolu_n",
        "bash",
        json!({ "command": "rm -rf ./build" }),
        Some("deletes outside the git tree — not reversible"),
        Some("mcp[claude-code]"),
        Some("   "),
    );
    let screen = play.render();
    assert!(
        screen.contains("escalated by claude-code"),
        "an MCP driver's escalation names the client, not its bracket spelling:\n{screen}"
    );
    assert!(
        screen.contains("deletes outside the git tree"),
        "with no note, the spotter's reason stays:\n{screen}"
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
