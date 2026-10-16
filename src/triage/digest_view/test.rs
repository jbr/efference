use super::*;
use crate::log::{AssistantOutput, ContentBlock, Keep, RawResult, Timestamp, ToolCall, Triaged};
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

/// An operator turn, one triaged call, then the triage's turn: the history every view
/// renders from, and the triage's call.
fn session() -> (Vec<LogEntry>, ToolCall) {
    let first = ToolCall::new("bash", "c1", json!({"command": "ls"})).with_intent("see the files");
    let triage =
        ToolCall::new("bash", "c2", json!({"command": "cargo test"})).with_intent("run the tests");
    let prefix = vec![
        entry(1, Payload::User(vec![ContentBlock::Text("fix it".into())])),
        assistant(2, "Looking around first.", vec![first]),
        entry(
            3,
            Payload::ToolResult {
                call_id: "c1".into(),
                raw: RawResult::text("a.rs\nb.rs"),
                wire: WireState::Triaged(Triaged::new(Keep::All).with_note("Two files.")),
            },
        ),
        assistant(4, "Now the tests.", vec![triage.clone()]),
    ];
    (prefix, triage)
}

/// The request's text, and its turns' parts.
fn render(view: TriageView) -> (String, Vec<Vec<UserPart>>) {
    let (prefix, triage) = session();
    let raw = RawResult::text("test result: ok. 3 passed");
    let pending = [Pending {
        call: &triage,
        raw: &raw,
    }];
    let turns = text_only_messages(&prefix, Reader::Main, view, &pending, &[], &[], &[]);
    let parts: Vec<Vec<UserPart>> = turns
        .iter()
        .map(|t| match t {
            WireTurn::User(parts) => parts.clone(),
            other => panic!("user turns only, no assistant turn: {other:?}"),
        })
        .collect();
    let text = parts
        .iter()
        .flatten()
        .filter_map(|p| match p {
            UserPart::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let (_, ask) = triage_parts(&pending, &[], &[], &[], Reader::Main, Framing::ThirdPerson);
    assert!(text.ends_with(&ask), "every view closes on the same ask");
    assert!(text.contains("cargo test") && text.contains("3 passed"));
    (text, parts)
}

/// `tail:1` carries only the turn before the triage's own; `intent` carries no history
/// at all. The triage's own prose rides the current step in both.
#[test]
fn the_tail_and_intent_views_cut_the_history() {
    let (tail, _) = render(TriageView::Tail(1));
    assert!(tail.contains("Now the tests.") && tail.contains("Two files."));
    assert!(
        !tail.contains("fix it"),
        "tail:1 carried the operator's turn"
    );

    let (intent, parts) = render(TriageView::Intent);
    assert_eq!(parts.len(), 1, "no history turn");
    for absent in ["fix it", "Looking around first.", "Two files."] {
        assert!(!intent.contains(absent), "intent carried {absent:?}");
    }
}

#[test]
fn views_parse_from_their_flag_spelling() {
    assert_eq!(TriageView::parse("text"), Some(TriageView::Text));
    assert_eq!(TriageView::parse("tail:3"), Some(TriageView::Tail(3)));
    assert_eq!(TriageView::parse("intent"), Some(TriageView::Intent));
    assert_eq!(TriageView::parse("digest"), None);
    assert_eq!(TriageView::parse("tail:0"), None);
    assert_eq!(TriageView::parse("tail"), None);
}

/// The shared triage reads the agent's transcript, so its fork reads the agent's cache;
/// a separate triage model reads the text view.
#[test]
fn the_view_follows_the_triage_configuration() {
    let endpoint = crate::api::Endpoint::from_credentials(
        crate::api::Provider::Anthropic,
        "sk-ant-xyz".into(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(Triager::shared().view(), TriageView::Transcript);
    let own = Triager::own(Client::new(endpoint.for_triage(), "small", "persona", None));
    assert_eq!(own.view(), TriageView::Text);
}
