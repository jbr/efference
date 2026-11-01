use super::*;
use crate::inference::api::Completion;
use crate::record::log::{
    AssistantOutput, ContentBlock, Keep, LoggedInference, Payload, RawResult, Summary, Timestamp,
    ToolCall, Triaged, WireState,
};
use crate::record::project::UserPart;
use serde_json::json;

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry::new(seq, payload)
        .with_turn(1)
        .with_at(Timestamp(1_700_000_000_000 + seq))
}

fn output(text: &str, calls: Vec<ToolCall>) -> AssistantOutput {
    let text = if text.is_empty() {
        vec![]
    } else {
        vec![ContentBlock::Text(text.into())]
    };
    AssistantOutput::from_legacy_messages(vec![], text, calls)
}

fn assistant(text: &str, calls: Vec<ToolCall>) -> Payload {
    Payload::assistant(output(text, calls))
}

/// Operator ask, one read, a reply, then a second read: seqs 1–6.
fn log() -> Vec<LogEntry> {
    let read = |id: &str| ToolCall::new("read", id, json!({"paths": ["src/a.rs"]}));
    let result = |id: &str| {
        Payload::tool_result(
            id,
            RawResult::text("body"),
            WireState::Triaged(Triaged::new(Keep::All).with_note("read it")),
        )
    };
    vec![
        entry(1, Payload::user("find the enum")),
        entry(2, assistant("", vec![read("c1")])),
        entry(3, result("c1")),
        entry(4, assistant("", vec![read("c2")])),
        entry(5, result("c2")),
        entry(6, assistant("It is in a.rs.", vec![])),
    ]
}

fn text(turns: &[WireTurn]) -> &str {
    match turns {
        [WireTurn::User(parts)] => match parts.as_slice() {
            [UserPart::Text(text)] => text,
            other => panic!("one text part expected, got {other:?}"),
        },
        other => panic!("one user turn expected, got {other:?}"),
    }
}

#[test]
fn the_request_is_the_digest_with_its_cut_then_the_prompt() {
    let turns = request(&log(), Some(3)).unwrap();
    let text = text(&turns);
    let digest = digest::render(
        &log(),
        digest::Options {
            cut: Some(3),
            ..Default::default()
        },
    );
    assert!(digest.contains(digest::CUT));
    assert_eq!(
        text,
        format!("{}\n\n---\n\n{}", digest.trim_end(), PROMPT.trim_end())
    );
}

#[test]
fn a_cut_between_a_call_and_its_result_is_refused() {
    match request(&log(), Some(4)) {
        Err(Error::SplitsCall { cut: 4, calls }) => assert_eq!(calls, vec!["c2".into()]),
        other => panic!("expected a split refusal, got {other:?}"),
    }
}

fn completion(text: &str, calls: Vec<ToolCall>, finish: Option<&str>) -> Completion {
    Completion::new(output(text, calls))
        .with_finish(finish.map(crate::inference::api::Finish::messages))
}

#[test]
fn the_account_is_the_reply_text_and_anything_else_is_an_error() {
    let written = completion("  The account.  ", vec![], Some("end_turn"));
    assert_eq!(account(&written).unwrap(), "The account.");

    let capped = completion("The acc", vec![], Some("max_tokens"));
    assert!(matches!(account(&capped), Err(Error::Truncated)));

    let call = ToolCall::new("bash", "c9", json!({"command": "ls"}));
    let called = completion("Let me look.", vec![call], Some("tool_use"));
    assert!(matches!(account(&called), Err(Error::CalledTool)));

    let empty = completion("  ", vec![], Some("end_turn"));
    assert!(matches!(account(&empty), Err(Error::Empty)));
}

#[test]
fn the_text_is_the_header_then_the_account() {
    let text = with_header("\nThe account.\n");
    assert!(text.starts_with("This is a handoff summary"));
    assert!(text.ends_with("---\n\nThe account."));
}

/// A second compaction reads the first one's account as context and covers only what
/// came after it: the account leads under its heading, without the harness's header,
/// and the digest starts past the earlier compaction's `upto`.
#[test]
fn a_second_compaction_reads_the_first_account_and_summarizes_only_after_it() {
    let mut entries = log();
    entries.insert(
        4,
        entry(
            7,
            Payload::Summary(Summary::compaction(
                3,
                with_header("The first account."),
                LoggedInference {
                    connection: "anthropic".into(),
                    provider: crate::record::log::Provider::Anthropic,
                    model: "claude-opus-5-5".into(),
                },
            )),
        ),
    );
    let turns = request(&entries, None).unwrap();
    let text = text(&turns);
    assert!(
        text.starts_with(&format!("{EARLIER}The first account.\n\n")),
        "{text}"
    );
    let after: Vec<LogEntry> = log().into_iter().filter(|e| e.seq > 3).collect();
    let digest = digest::render(&after, digest::Options::default());
    assert!(text.contains(digest.trim_end()), "{text}");
    assert_eq!(
        text.matches("find the enum").count(),
        0,
        "the operator turn before the earlier compaction is not summarized again"
    );
}
