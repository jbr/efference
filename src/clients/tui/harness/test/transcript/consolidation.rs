//! A consolidation as the web client draws it (docs/tui.md): the resolved call as its
//! bare name, then a rule naming the extent, the summary as markdown, a closing rule.

use super::*;
use crate::record::log::{Payload, Summary};

#[test]
fn a_consolidation_reads_as_its_extent_and_summary() {
    let mut play = Screenplay::new(80, 30);
    play.user("Groom the backlog.");
    play.assistant("Reading the rules.", vec![]);
    let summary = "# Findings so far\n\n- first-run/ is done\n- web/ has two live units";
    play.assistant(
        "",
        vec![ToolCall::new(
            "consolidate",
            "toolu_c",
            json!({"summary": summary, "intent": "fold the reading"}),
        )],
    );
    play.commit(Payload::Summary(Summary::consolidation(
        0,
        2,
        summary.to_string(),
    )));
    play.tool_result("toolu_c", "consolidated", false, WireState::Raw);

    let frame = play.render();
    assert!(
        frame.contains("› consolidate") && !frame.contains("summary"),
        "the resolved call is its bare name, not its input again:\n{frame}"
    );
    assert!(
        frame.contains("⊙ consolidated through #2 · 2 entries"),
        "the rule names the extent in entries:\n{frame}"
    );
    assert!(
        frame.contains("Findings so far") && !frame.contains("# Findings"),
        "the summary renders as markdown:\n{frame}"
    );
    play.snapshot("transcript_consolidation");
}

/// A consolidation adopted while the trajectory kept going is appended after the
/// later work, but is drawn where the agent's projection puts its account: after
/// the entry its range ends in (docs/consolidate.md). Rendered once before the summary
/// arrives, so the incremental view has to move it into an entry already drawn.
#[test]
fn a_late_consolidation_is_drawn_at_its_range_end() {
    let mut play = Screenplay::new(80, 40);
    play.user("Survey the backlog.");
    play.assistant(
        "",
        vec![ToolCall::new(
            "read",
            "toolu_r",
            json!({"path": "backlog/INDEX.md", "intent": "survey"}),
        )],
    );
    play.tool_result("toolu_r", "index contents", false, WireState::Raw);
    play.assistant("The survey is complete.", vec![]);
    play.user("Now group them.");
    play.assistant(
        "",
        vec![ToolCall::new(
            "list",
            "toolu_l",
            json!({"path": "backlog/web", "intent": "later work"}),
        )],
    );
    play.render();
    play.commit(Payload::Summary(Summary::consolidation(
        1,
        3,
        "Account of the survey.",
    )));

    let frame = play.render();
    let at = |needle: &str| {
        frame
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing:\n{frame}"))
    };
    assert!(
        frame.contains("⊙ consolidated #1–#3 · 3 entries"),
        "the rule names the range and counts its turns and results only:\n{frame}"
    );
    assert!(
        at("survey") < at("⊙ consolidated")
            && at("⊙ consolidated") < at("The survey is complete")
            && at("The survey is complete") < at("later work"),
        "the account sits at the end of its range, ahead of the work after it:\n{frame}"
    );
}
