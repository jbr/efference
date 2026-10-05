//! A consolidation as the web client draws it (docs/tui.md): the resolved call as its
//! bare name, then a rule naming the extent, the summary as markdown, a closing rule.

use super::*;
use crate::log::{Mark, Payload};

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
    play.commit(Payload::Mark(Mark::boundary(2, summary.to_string())));
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
