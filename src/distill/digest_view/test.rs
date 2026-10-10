use super::*;
use crate::log::{AssistantOutput, ContentBlock, Distilled, Keep, RawResult, Timestamp, ToolCall};
use serde_json::json;

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 1,
        at: Timestamp(1_700_000_000_000 + seq),
        submission_id: None,
        source: None,
        payload,
    }
}

fn assistant(seq: u64, text: &str, calls: Vec<ToolCall>) -> LogEntry {
    entry(
        seq,
        Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(
                vec![],
                vec![ContentBlock::Text(text.into())],
                calls,
            ),
            upstream_failure: None,
        },
    )
}

/// An operator turn, one triaged call, then the gate's turn: the history every view
/// renders from, and the gate's call.
fn session() -> (Vec<LogEntry>, ToolCall) {
    let first = ToolCall::new("bash", "c1", json!({"command": "ls"})).with_intent("see the files");
    let gate =
        ToolCall::new("bash", "c2", json!({"command": "cargo test"})).with_intent("run the tests");
    let prefix = vec![
        entry(1, Payload::User(vec![ContentBlock::Text("fix it".into())])),
        assistant(2, "Looking around first.", vec![first]),
        entry(
            3,
            Payload::ToolResult {
                call_id: "c1".into(),
                raw: RawResult::text("a.rs\nb.rs"),
                wire: WireState::Distilled(Distilled::new(Keep::All).with_note("Two files.")),
            },
        ),
        assistant(4, "Now the tests.", vec![gate.clone()]),
    ];
    (prefix, gate)
}

/// The request's text, and its parts.
fn render(view: TriageView) -> (String, Vec<UserPart>) {
    let (prefix, gate) = session();
    let raw = RawResult::text("test result: ok. 3 passed");
    let pending = [Pending {
        call: &gate,
        raw: &raw,
    }];
    let turns = digest_peek_messages(&prefix, Reader::Main, view, &pending, &[], &[], &[]);
    let [WireTurn::User(parts)] = turns.as_slice() else {
        panic!("one user turn, no assistant turn: {turns:?}");
    };
    let text = parts
        .iter()
        .filter_map(|p| match p {
            UserPart::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let (_, ask) = gate_parts(&pending, &[], &[], &[]);
    assert!(text.ends_with(&ask), "every view closes on the same ask");
    assert!(text.contains("cargo test") && text.contains("3 passed"));
    (text, parts.clone())
}

/// The digest view sends text only — no assistant turn, no tool block — so it encodes on
/// any dialect. It carries the earlier call as its digest line, the gate's call with its
/// arguments, the gate's result, and the same ask the transcript view sends.
#[test]
fn the_digest_view_is_text_with_the_digest_the_gate_and_the_ask() {
    let (text, parts) = render(TriageView::Digest);
    assert!(
        parts
            .iter()
            .all(|p| !matches!(p, UserPart::ToolResult { .. })),
        "no tool blocks"
    );
    assert!(
        matches!(parts[1], UserPart::Breakpoint),
        "the breakpoint follows the digest"
    );
    for expected in [
        "fix it",
        "Looking around first.",
        "• [bash] see the files",
        "→ Two files.",
        "Now the tests.",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
}

/// `tail:1` carries only the gate's own turn; `intent` carries no history at all.
#[test]
fn the_tail_and_intent_views_cut_the_history() {
    let (tail, _) = render(TriageView::Tail(1));
    assert!(tail.contains("Now the tests."));
    for absent in ["fix it", "Looking around first.", "Two files."] {
        assert!(!tail.contains(absent), "tail:1 carried {absent:?}");
    }
    let (wider, _) = render(TriageView::Tail(2));
    assert!(wider.contains("Looking around first.") && wider.contains("Two files."));

    let (intent, parts) = render(TriageView::Intent);
    assert!(!parts.iter().any(|p| matches!(p, UserPart::Breakpoint)));
    for absent in ["fix it", "Looking around first.", "Now the tests."] {
        assert!(!intent.contains(absent), "intent carried {absent:?}");
    }
}

#[test]
fn views_parse_from_their_flag_spelling() {
    assert_eq!(TriageView::parse("tail:3"), Some(TriageView::Tail(3)));
    assert_eq!(TriageView::parse("intent"), Some(TriageView::Intent));
    assert_eq!(TriageView::parse("tail:0"), None);
    assert_eq!(TriageView::parse("tail"), None);
}
