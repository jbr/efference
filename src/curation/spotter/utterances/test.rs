use super::*;
use crate::config::Config;
use crate::curation::spotter::{Circumstances, request_turns};
use crate::protocol::agents::Supervision;
use crate::record::log::{AssistantOutput, ContentBlock, LogEntry, Payload, ToolCall};
use crate::record::names::tools::BASH;
use crate::record::project::{UserPart, WireTurn};
use crate::record::utterances::utterances;

/// A log of operator turns and session replies, in order.
fn log(steps: &[(&str, &str)]) -> Vec<LogEntry> {
    let mut entries = Vec::new();
    for (who, text) in steps {
        let seq = entries.len() as u64;
        let entry = match *who {
            "session" => LogEntry::new(
                seq,
                Payload::assistant(AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![ContentBlock::Text(text.to_string())],
                    vec![],
                )),
            ),
            "operator" => LogEntry::new(seq, Payload::user(*text)),
            source => {
                LogEntry::new(seq, Payload::user(*text)).with_source(Some(source.to_string()))
            }
        };
        entries.push(entry);
    }
    entries
}

/// The block for a short conversation, byte for byte: each utterance a part, with the
/// end of the session's text it answers above it when there is one.
#[test]
fn a_short_conversation_renders_exactly() {
    let entries = log(&[
        (
            "operator",
            "Load the gallery at http://localhost:5173 in Playwright.",
        ),
        ("session", "Loaded. Want me to try the dark theme too?"),
        ("operator", "yes"),
        ("session[lead]", "Report back when the gallery passes."),
    ]);
    assert_eq!(
        parts(&utterances(&entries)),
        vec![
            "What the operator has said in this session, oldest first:\n\n",
            "Operator:\nLoad the gallery at http://localhost:5173 in Playwright.\n\n",
            "[the session had just written]\nLoaded. Want me to try the dark theme too?\n\
             Operator:\nyes\n\n",
            "Supervising session `lead`:\nReport back when the gallery passes.\n\n",
        ]
    );
}

/// A session nobody has spoken to yet says so.
#[test]
fn no_utterances_render_as_nothing_before_this_move() {
    assert_eq!(
        parts(&[]),
        vec![
            "What the operator has said in this session, oldest first:\n\n",
            "(nothing before this move)\n\n",
        ]
    );
}

/// A later call's block starts with an earlier call's, part for part: the session's
/// reply and the operator's next turn only append.
#[test]
fn appending_to_the_log_appends_to_the_block() {
    let mut steps = vec![
        ("operator", "Fix the flaky test."),
        ("session", "Found it: a race in the watcher."),
        ("operator", "Go ahead and patch it."),
    ];
    let before = parts(&utterances(&log(&steps)));
    steps.push(("session", "Patched; shall I run the whole suite?"));
    steps.push(("operator", "yes, and web search the error if it recurs"));
    let after = parts(&utterances(&log(&steps)));
    assert!(after.len() > before.len());
    assert_eq!(&after[..before.len()], &before[..]);
}

/// Utterances of `size` characters each, `count` of them, the first marked.
fn many(count: usize, size: usize) -> Vec<Utterance> {
    (0..count)
        .map(|i| {
            let text = format!("{i:04} {}", "x".repeat(size));
            Utterance::new(Speaker::Operator, &text, None)
        })
        .collect()
}

fn part_chars(parts: &[String]) -> usize {
    parts[1..].iter().map(|part| part.chars().count()).sum()
}

/// Past the budget the block rolls over: the first utterance stays, a marker counts the
/// ones left out, and the most recent follow, within half the budget. The same log
/// renders the same bytes, and the block appends again from the rollover.
#[test]
fn past_the_budget_the_block_rolls_over() {
    // Each part is 1017 characters: 23 fit, and the 24th rolls the block over.
    let utterances = many(30, 1000);
    let rendered = parts(&utterances);
    assert_eq!(rendered, parts(&utterances), "deterministic");
    assert!(rendered[1].contains("0000 "), "the first utterance stays");
    let marker = rendered
        .iter()
        .find(|part| part.starts_with("[…"))
        .expect("a marker");
    assert_eq!(
        rendered[2], *marker,
        "the marker follows the first utterance"
    );
    assert!(part_chars(&rendered) <= BUDGET);
    assert!(
        rendered.last().unwrap().contains("0029 "),
        "the newest is kept"
    );
    let kept = rendered.len() - 3;
    assert_eq!(
        marker,
        &format!("[… {} earlier turns left out …]\n\n", 30 - 1 - kept)
    );

    // Within the rollover, appending appends.
    let more = parts(&many(31, 1000));
    assert_eq!(&more[..rendered.len()], &rendered[..]);
    assert_eq!(more.len(), rendered.len() + 1);
}

/// The parts of the request the prose rung sends for a bash move.
fn request_parts(circumstances: &Circumstances<'_>) -> Vec<UserPart> {
    let call = ToolCall::new(BASH, "toolu_1", serde_json::json!({ "command": "ls" }));
    let turns = request_turns(&Config::default(), &call, circumstances, None, None);
    let [WireTurn::User(parts)] = &turns[..] else {
        panic!("one user turn: {turns:?}");
    };
    parts.clone()
}

/// The utterances sit between the scope block's breakpoint and a second one, ahead of
/// the facts and the move, which change between calls.
#[test]
fn the_utterances_sit_between_the_two_breakpoints() {
    let said = utterances(&log(&[
        ("operator", "Fix the flaky test."),
        ("operator", "and web search the error"),
    ]));
    let supervision = Supervision::default();
    let parts = request_parts(
        &Circumstances::default()
            .with_utterances(Some(&said))
            .with_supervision(Some(&supervision)),
    );
    let [
        UserPart::Text(scope),
        UserPart::Breakpoint,
        UserPart::Text(heading),
        UserPart::Text(first),
        UserPart::Text(second),
        UserPart::Breakpoint,
        UserPart::Text(facts),
        UserPart::Text(the_move),
    ] = &parts[..]
    else {
        panic!("scope, ◆, utterances, ◆, facts, move: {parts:?}");
    };
    assert!(scope.starts_with("Mission:"));
    assert_eq!(heading, HEADING);
    assert!(first.ends_with("Fix the flaky test.\n\n"));
    assert!(second.ends_with("and web search the error\n\n"));
    assert!(facts.starts_with("Facts the harness records"));
    assert!(the_move.contains("The session is about to make this move"));

    // Without the facts, the move follows the second breakpoint.
    let parts = request_parts(&Circumstances::default().with_utterances(Some(&said)));
    assert!(
        matches!(&parts[parts.len() - 2..], [UserPart::Breakpoint, UserPart::Text(m)] if m.contains("about to make this move")),
        "{parts:?}"
    );
}

/// With the utterances not known, there is no block and no second breakpoint.
#[test]
fn unknown_utterances_render_no_block() {
    let parts = request_parts(&Circumstances::default());
    assert_eq!(
        parts
            .iter()
            .filter(|part| matches!(part, UserPart::Breakpoint))
            .count(),
        1,
        "{parts:?}"
    );
    assert!(
        !parts
            .iter()
            .any(|part| matches!(part, UserPart::Text(t) if t.starts_with("What the operator"))),
        "{parts:?}"
    );
}

/// A bound leaf's move says whose conversation is shown: the approving session's, with
/// the holder working under it.
#[test]
fn a_bound_leaf_says_whose_conversation_is_shown() {
    let child = ["child".to_string()];
    let parts = request_parts(&Circumstances::default().with_chain(&child));
    let UserPart::Text(the_move) = parts.last().unwrap() else {
        panic!("{parts:?}");
    };
    assert!(
        the_move.contains(
            "What the operator has said, above, was said to the session approving the move, \
             and `child` is a session working under it."
        ),
        "{the_move}"
    );

    let chain = ["mid".to_string(), "leaf".to_string()];
    let parts = request_parts(&Circumstances::default().with_chain(&chain));
    let UserPart::Text(the_move) = parts.last().unwrap() else {
        panic!("{parts:?}");
    };
    assert!(the_move.contains("mid → leaf"), "{the_move}");
    assert!(
        the_move.contains(
            "`mid` is a session working under it, and each session in the chain works under \
             the one before it."
        ),
        "{the_move}"
    );
}
