use super::*;
use crate::log::{
    AssistantOutput, ConfigEdit, ContentBlock, Disposition, Distilled, Keep, Notification, Origin,
    Timestamp, ToolTelemetry, Triage,
};

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

/// A sourced `User` entry opens its line with the same `source:` address the
/// wire carries (docs/control-api.md); the unsourced line above stays bare.
#[test]
fn a_sourced_user_line_shows_the_source() {
    let mut user = entry(
        1,
        Payload::User(vec![ContentBlock::Text("please look".into())]),
    );
    user.source = Some("mcp[claude-code]".into());
    assert_eq!(
        render(&[user]),
        "      1 user      mcp[claude-code]: please look\n"
    );
}

fn assistant(text: &str, calls: Vec<ToolCall>) -> Payload {
    let text = if text.is_empty() {
        vec![]
    } else {
        vec![ContentBlock::Text(text.into())]
    };
    Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(vec![], text, calls),
        upstream_failure: None,
    }
}

#[test]
fn a_turn_renders_prose_calls_and_verdicts_without_bodies() {
    let big = (1..=40)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let log = [
        entry(
            1,
            Payload::User(vec![ContentBlock::Text("find the enum".into())]),
        ),
        entry(
            2,
            assistant(
                "",
                vec![
                    ToolCall::new(
                        "read",
                        "toolu_1",
                        serde_json::json!({ "paths": ["src/log.rs"] }),
                    )
                    .with_intent("Find WireState"),
                ],
            ),
        ),
        entry(
            3,
            Payload::tool_result(
                "toolu_1",
                RawResult::text(big.clone()),
                Distilled::new(Keep::Ranges(vec![(10, 14)]))
                    .with_note("the enum")
                    .into(),
            ),
        ),
        entry(
            4,
            assistant(
                "Four variants.\nRaw, Distilled, Elided, TriageFailed.",
                vec![],
            ),
        ),
    ];
    let text = render(&log);
    assert_eq!(
        text,
        "      1 user      find the enum\n\
         \x20     2 call      read paths=\"[\\\"src/log.rs\\\"]\"  intent: Find WireState\n\
         \x20     3 result    ok 310 B, kept 5/40 lines: the enum\n\
         \x20     4 assistant Four variants.\n\
         \x20                 Raw, Distilled, Elided, TriageFailed.\n"
    );
    assert!(
        !text.contains("line 12"),
        "a body never rides the transcript"
    );
}

#[test]
fn errors_raw_and_images_are_named_not_shown() {
    let log = [
        entry(
            1,
            Payload::tool_result("toolu_1", RawResult::error("boom"), WireState::Raw),
        ),
        entry(
            2,
            Payload::tool_result(
                "toolu_2",
                RawResult::text("ok").with_images(vec![crate::log::ResultImage {
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                    width: 1,
                    height: 1,
                }]),
                WireState::Elided,
            ),
        ),
    ];
    let text = render(&log);
    assert!(
        text.contains("      1 result    ERROR 4 B, raw\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "      2 result    ok 2 B, elided; 1 image(s), fetch with `image` at this seq\n"
        ),
        "{text}"
    );
}

#[test]
fn silent_entries_render_to_nothing_and_verdicts_on_the_inbox_do_not() {
    let quiet = Triage {
        for_result: Some(3),
        origin: Origin::Model,
        raw_keep: "all".into(),
        inbox: vec![],
        surfaced: vec![],
        retries: vec![],
        thinking: vec![],
    };
    let mut loud = quiet.clone();
    loud.inbox = vec![(9, Disposition::Dismiss)];
    loud.retries = vec!["attempt 1 of 3: peek called bash".into()];
    let log = [
        entry(1, Payload::Triage(quiet)),
        entry(
            2,
            Payload::ToolTelemetry(ToolTelemetry {
                call_id: "toolu_1".into(),
                tool: "read".into(),
                duration_ms: 1,
                outcome: crate::log::ToolOutcome::Applied,
                result_bytes: 1,
                images: 0,
                bash: None,
                edit: None,
            }),
        ),
        entry(3, Payload::Triage(loud)),
        entry(
            4,
            Payload::Ambient(Notification {
                source: "agent:fable".into(),
                body: "review level changed".into(),
                slot: None,
            }),
        ),
    ];
    assert_eq!(
        render(&log),
        "      3 triage    dismissed notification #9; retry: attempt 1 of 3: peek called bash\n\
         \x20     4 notify    [agent:fable] review level changed\n"
    );
}

#[test]
fn a_config_proposal_names_its_change_and_its_resolution() {
    let log = [
        entry(
            5,
            Payload::ConfigEdit(ConfigEdit {
                state: EditState::Proposed,
                resolves: None,
                change: ConfigChange::Command {
                    command: "grep -n".into(),
                },
                note: "spotter: judged safe".into(),
            }),
        ),
        entry(
            6,
            Payload::ConfigEdit(ConfigEdit {
                state: EditState::Committed,
                resolves: Some(5),
                change: ConfigChange::Command {
                    command: "grep -n".into(),
                },
                note: String::new(),
            }),
        ),
    ];
    assert_eq!(
        render(&log),
        "      5 config    proposed command \"grep -n\": spotter: judged safe\n\
         \x20     6 config    committed command \"grep -n\" (resolves #5)\n"
    );
}

#[test]
fn a_long_argument_is_clipped_and_a_newline_flattened() {
    let contents = "x".repeat(ARG_CAP + 10);
    let call = ToolCall::new(
        "write",
        "toolu_1",
        serde_json::json!({ "path": "a.rs", "contents": format!("{contents}\nmore") }),
    );
    let line = call_line(&call);
    assert!(line.starts_with("write contents=\"xxx"), "{line}");
    assert!(line.contains("…\" path=\"a.rs\""), "{line}");
    assert!(!line.contains('\n'));
}

/// A `command` is what a gate is decided on, so it is not cut where a body is.
#[test]
fn a_command_gets_room_where_a_body_does_not() {
    let command = format!("cargo test -- {}", "very_long_test_name_".repeat(8));
    assert!(command.len() > ARG_CAP && command.len() < ARG_WIDE);
    let call = ToolCall::new("bash", "toolu_1", serde_json::json!({ "command": command }));
    assert_eq!(call_line(&call), format!("bash command={command:?}"));
}

#[test]
fn a_title_shows_when_it_moves_and_not_when_it_is_kept() {
    let title = |text: &str| {
        Payload::Title(crate::log::Title {
            text: text.into(),
            author: crate::log::TitleAuthor::Triage,
        })
    };
    let log = [
        entry(1, title("Testing the harness")),
        entry(2, title("Testing the harness")),
        entry(3, title("Adding a test for latest_title")),
    ];
    assert_eq!(
        render(&log),
        "      1 title     Testing the harness\n\
         \x20     3 title     Adding a test for latest_title\n"
    );
}

/// Two calls in one entry are `seq.1` and `seq.2`, and each result names its call;
/// a lone call keeps the bare seq and its result says nothing extra.
#[test]
fn sibling_calls_are_numbered_and_their_results_name_them() {
    let log = [
        entry(
            2,
            assistant(
                "",
                vec![
                    ToolCall::new("read", "toolu_a", serde_json::json!({ "paths": ["a.rs"] })),
                    ToolCall::new("list", "toolu_b", serde_json::json!({ "path": "src" })),
                ],
            ),
        ),
        entry(
            3,
            Payload::tool_result("toolu_a", RawResult::text("fn a() {}"), WireState::Raw),
        ),
        entry(
            5,
            Payload::tool_result("toolu_b", RawResult::text("a.rs"), WireState::Raw),
        ),
        entry(
            6,
            assistant(
                "",
                vec![ToolCall::new("read", "toolu_c", serde_json::json!({}))],
            ),
        ),
        entry(
            7,
            Payload::tool_result("toolu_c", RawResult::text("x"), WireState::Raw),
        ),
    ];
    assert_eq!(
        render(&log),
        "    2.1 call      read paths=\"[\\\"a.rs\\\"]\"\n\
         \x20   2.2 call      list path=\"src\"\n\
         \x20     3 result    for 2.1: ok 9 B, raw\n\
         \x20     5 result    for 2.2: ok 4 B, raw\n\
         \x20     6 call      read\n\
         \x20     7 result    ok 1 B, raw\n"
    );
}
