//! Tests for what a turn leaves on the wire.
use super::*;

/// The wire shape an interrupt produces (docs/control-channel.md). Three
/// things have to hold at once, and they are the whole design:
///
/// 1. The `tool_use` is **paired** — an orphan is a request the API rejects.
/// 2. The pair carries **none of the output** and no harness gloss: the point
///    of interrupting is that the tool's output never enters the context, and
///    a stand-in explaining the situation would be the harness guessing at a
///    meaning that differs per tool.
/// 3. The operator's next message merges into the **same** user message, so
///    the model reads `[tool_result, "…"]` in one breath — the person who
///    interrupted says why, in their own words, right where the output would
///    have been. That merge is `project`'s existing seam-drain behavior; this
///    pins that it survives the turn boundary an interrupt introduces.
#[test]
pub(super) fn an_interrupted_call_is_paired_but_says_nothing_and_the_operator_speaks_next() {
    use crate::project::ApiBlock;
    let (shared, _rx) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    let call = bash_call("cat enormous.log");
    shared
        .append(
            1,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(vec![], vec![], vec![call.clone()]),
                upstream_failure: None,
            },
        )
        .unwrap();
    shared
        .append(
            1,
            withheld(
                &call,
                RawResult::text("SECRET_TOKEN=hunter2\n…120k of log…"),
            ),
        )
        .unwrap();
    // The turn ended; the operator's message opens the next one.
    shared
        .append(
            2,
            Payload::User(vec![ContentBlock::Text("read the test instead".into())]),
        )
        .unwrap();

    let messages = shared.project_messages();
    let last = messages.last().expect("a projected message");
    assert_eq!(last.role, crate::project::Role::User);
    match &last.content[..] {
        [
            ApiBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            },
            ApiBlock::Text { text, .. },
        ] => {
            assert_eq!(tool_use_id, &call.id.0, "the tool_use is paired");
            assert!(
                content.contains("[elided]"),
                "the pair is a placeholder: {content}"
            );
            assert!(
                !content.contains("hunter2") && !content.contains("120k"),
                "the withheld output never reaches the wire: {content}"
            );
            assert!(!is_error, "the call didn't fail — it was interrupted");
            assert_eq!(text, "read the test instead");
        }
        other => panic!("expected [tool_result, text] in one user message, got {other:?}"),
    }

    // Withheld from the model, not from the operator: `raw` is intact in the
    // log, so the TUI can still show it and the verdict stays reversible.
    let entries = shared.snapshot();
    let logged = entries
        .iter()
        .find_map(|e| match &e.payload {
            Payload::ToolResult { raw, wire, .. } => Some((raw, wire)),
            _ => None,
        })
        .expect("the result is in the log");
    assert!(logged.0.content.contains("hunter2"), "raw survives the log");
    assert!(matches!(logged.1, WireState::Elided));
}

/// A completion that carried several calls produces a **fully-paired** log, and
/// `project` merges the results into one user message (docs/README.md).
///
/// One tool call per turn is *requested* of the provider via
/// `disable_parallel_tool_use`, but it is only a request: no OpenRouter-routed endpoint
/// lists the equivalent knob in its supported parameters, so a provider may return
/// several calls and the flag is ignored by design. The loop therefore has to handle
/// N ≥ 1 rather than assume N = 1 — every `tool_use` gets exactly one `tool_result`, or
/// projection orphans it and the API rejects the request.
#[test]
pub(super) fn a_multi_call_completion_projects_a_fully_paired_wire() {
    use crate::project::ApiBlock;
    let (shared, _rx) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
    let first = ToolCall::new("search", "search_2", serde_json::json!({}));
    let second = ToolCall::new(
        crate::bash::NAME,
        "bash_3",
        serde_json::json!({ "command": "ls" }),
    );
    shared
        .append(
            1,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![first.clone(), second.clone()],
                ),
                upstream_failure: None,
            },
        )
        .unwrap();
    for (call, content) in [(&first, "hits"), (&second, "listing")] {
        shared
            .append(
                1,
                Payload::tool_result(call.id.clone(), RawResult::text(content), WireState::Raw),
            )
            .unwrap();
    }

    let messages = shared.project_messages();
    let assistant = &messages[messages.len() - 2];
    let uses: Vec<&str> = assistant
        .content
        .iter()
        .filter_map(|b| match b {
            ApiBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(uses, vec!["search_2", "bash_3"]);

    // Both results land in a *single* trailing user message, in call order — the shape
    // Anthropic expects for a multi-call turn, and the one the fork peek reproduces.
    let last = messages.last().expect("a trailing user message");
    assert_eq!(last.role, crate::project::Role::User);
    let answered: Vec<&str> = last
        .content
        .iter()
        .filter_map(|b| match b {
            ApiBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        answered, uses,
        "every tool_use is answered exactly once, in order"
    );
}

/// The title peek's trigger (docs/session-title.md): ask when no title exists,
/// once after each user turn, and on the trajectory-count fallback otherwise.
#[test]
fn title_is_solicited_after_a_user_turn_and_on_the_fallback() {
    use crate::log::{LogEntry, Timestamp, Title, TitleAuthor};
    fn entry(seq: u64, payload: Payload) -> LogEntry {
        LogEntry {
            seq,
            turn: 0,
            at: Timestamp(seq),
            submission_id: None,
            source: None,
            payload,
        }
    }
    let user = || Payload::User(vec![ContentBlock::Text("go".into())]);
    let assistant = || Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(
            vec![],
            vec![ContentBlock::Text("done".into())],
            vec![],
        ),
        upstream_failure: None,
    };
    let title = || {
        Payload::Title(Title {
            text: "t".into(),
            author: TitleAuthor::Triage,
        })
    };

    // Never titled: ask.
    let mut log = vec![entry(0, user()), entry(1, assistant())];
    assert!(should_solicit_title(&log));
    // Titled after that answer: nothing new to ask about.
    log.push(entry(2, title()));
    assert!(!should_solicit_title(&log));
    // Autonomous continuation without a user turn: not until the fallback.
    for seq in 3..3 + (TITLE_FALLBACK_TRAJECTORIES as u64 - 1) {
        log.push(entry(seq, assistant()));
        assert!(!should_solicit_title(&log), "{seq}");
    }
    log.push(entry(100, assistant()));
    assert!(should_solicit_title(&log), "the fallback count is reached");
    // A kept title resets the fallback; a user turn after it asks again.
    log.push(entry(101, title()));
    assert!(!should_solicit_title(&log));
    log.push(entry(102, user()));
    log.push(entry(103, assistant()));
    assert!(should_solicit_title(&log));
}

/// A must-wake notification is pinned whatever the peek decided (docs/inbox.md):
/// a dismiss is overridden, a leave is filled in, and an ordinary item's verdict
/// is untouched.
#[test]
fn force_pins_overrides_the_peek_for_must_wake_items() {
    let mut inbox = vec![
        (5, Disposition::Dismiss),
        (6, Disposition::Pin),
        (7, Disposition::Dismiss),
    ];
    force_pins(&mut inbox, &[5, 8]);
    inbox.sort_by_key(|(seq, _)| *seq);
    assert_eq!(
        inbox,
        vec![
            (5, Disposition::Pin),
            (6, Disposition::Pin),
            (7, Disposition::Dismiss),
            (8, Disposition::Pin),
        ]
    );
    let mut untouched = vec![(1, Disposition::Dismiss)];
    force_pins(&mut untouched, &[]);
    assert_eq!(untouched, vec![(1, Disposition::Dismiss)]);
}
