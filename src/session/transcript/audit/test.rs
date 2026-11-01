use super::*;
use crate::record::log::Telemetry;
use crate::record::log::{
    AssistantOutput, ConfigEdit, ContentBlock, Disposition, Keep, Notification, Timestamp,
    ToolTelemetry, Triage, Triaged,
};

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry::new(seq, payload)
        .with_turn(1)
        .with_at(Timestamp(1_700_000_000_000 + seq))
}

/// A sourced `User` entry opens its line with the same `source:` address the
/// wire carries (docs/control-api.md); the unsourced line above stays bare.
#[test]
fn a_sourced_user_line_shows_the_source() {
    let mut user = entry(1, Payload::user("please look"));
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
    Payload::assistant(AssistantOutput::from_legacy_messages(vec![], text, calls))
}

#[test]
fn a_turn_renders_prose_calls_and_verdicts_without_bodies() {
    let big = (1..=40)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let log = [
        entry(1, Payload::user("find the enum")),
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
                Triaged::new(Keep::Ranges(vec![(10, 14)]))
                    .with_note("the enum")
                    .into(),
            ),
        ),
        entry(
            4,
            assistant(
                "Four variants.\nRaw, Triaged, Elided, TriageFailed.",
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
         \x20                 Raw, Triaged, Elided, TriageFailed.\n"
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
                RawResult::text("ok").with_images(vec![crate::record::log::ResultImage {
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
    let quiet = Triage::new(Some(3), "all");
    let mut loud = quiet.clone();
    loud.inbox = vec![(9, Disposition::Dismiss)];
    loud.retries = vec!["attempt 1 of 3: triage called bash".into()];
    let log = [
        entry(1, Payload::Triage(quiet)),
        entry(
            2,
            Payload::Telemetry(Telemetry::Tool(ToolTelemetry {
                call_id: "toolu_1".into(),
                tool: "read".into(),
                duration_ms: 1,
                outcome: crate::record::log::ToolOutcome::Applied,
                result_bytes: 1,
                images: 0,
                bash: None,
                edit: None,
            })),
        ),
        entry(3, Payload::Triage(loud)),
        entry(
            4,
            Payload::Ambient(Notification::new(
                "agent:fable",
                "review level changed",
                None,
            )),
        ),
    ];
    assert_eq!(
        render(&log),
        "      3 triage    dismissed notification #9; retry: attempt 1 of 3: triage called bash\n\
         \x20     4 notify    [agent:fable] review level changed\n"
    );
}

#[test]
fn a_config_proposal_names_its_change_and_its_resolution() {
    let log = [
        entry(
            5,
            Payload::ConfigEdit(
                ConfigEdit::new(
                    EditState::Proposed,
                    ConfigChange::Command {
                        command: "grep -n".into(),
                    },
                )
                .with_note("spotter: judged safe".into()),
            ),
        ),
        entry(
            6,
            Payload::ConfigEdit(
                ConfigEdit::new(
                    EditState::Committed,
                    ConfigChange::Command {
                        command: "grep -n".into(),
                    },
                )
                .with_resolves(Some(5)),
            ),
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
        Payload::Title(crate::record::log::Title::new(
            text,
            crate::record::log::TitleAuthor::Triage,
        ))
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

/// A pending or answered `ask` reads as the assistant's question and the operator's
/// reply, not as a call and a result (docs/control-api.md); the position shows only
/// when there is more than one question.
#[test]
fn an_ask_reads_as_a_question_and_its_reply() {
    let ask = |id: &str, q: &str| ToolCall::new("ask", id, serde_json::json!({ "question": q }));
    let mut reply = entry(
        3,
        Payload::tool_result("toolu_a", RawResult::text("yes, go"), WireState::Raw),
    );
    reply.submission_id = Some("sub-1".into());
    let mut sourced = entry(
        6,
        Payload::tool_result("toolu_b", RawResult::text("no"), WireState::Raw),
    );
    sourced.submission_id = Some("sub-2".into());
    sourced.source = Some("web".into());
    let log = [
        entry(2, assistant("Ready.", vec![ask("toolu_a", "Ship it?")])),
        reply,
        entry(
            5,
            assistant(
                "",
                vec![ask("toolu_b", "First?"), ask("toolu_c", "Second?")],
            ),
        ),
        sourced,
        entry(
            7,
            Payload::tool_result(
                "toolu_c",
                RawResult::error("ask interrupted by user"),
                WireState::Raw,
            ),
        ),
    ];
    assert_eq!(
        render(&log),
        "\x20     2 assistant Ready.\n\
         \x20     2 asked     Ship it?\n\
         \x20     3 user      yes, go\n\
         \x20   5.1 asked     First? (question 1 of 2)\n\
         \x20   5.2 asked     Second? (question 2 of 2)\n\
         \x20     6 user      web: no\n\
         \x20     7 result    not answered\n"
    );
}

/// Each mechanism entry is one line under its own tag: a reply kept record-only with
/// why, a summary with its extent and text, a spotter verdict with its stance, the
/// endpoint's image refusal, a process's mint and exit, the system prompt's size, a
/// captured result's verdict, a restart notice, and a deliberate archive.
#[test]
fn each_mechanism_entry_has_its_line() {
    use crate::record::log::{
        ExitOutcome, ProcessEvent, ProcessEventKind, ProcessExit, ProcessSpawn, Resumed, Spot,
        Summary, WireDialect,
    };
    let process = |kind| Payload::ProcessEvent(ProcessEvent { id: 2, kind });
    let log = [
        entry(
            1,
            Payload::record_only(
                AssistantOutput::empty(WireDialect::Messages),
                crate::record::log::Exclusion::Empty,
            ),
        ),
        entry(
            2,
            Payload::Summary(Summary::consolidation(0, 1, "the opening")),
        ),
        entry(
            3,
            Payload::Spot(
                Spot::new("toolu_1", Stance::Watch, "outside the scope")
                    .with_probed_prefix(Some("cargo".into())),
            ),
        ),
        entry(
            4,
            Payload::ImagesRefused {
                refusal: "image input is not supported".into(),
            },
        ),
        entry(
            5,
            process(ProcessEventKind::Spawned(ProcessSpawn {
                command: "cargo watch".into(),
                call_id: None,
                pid: None,
                at: Timestamp(0),
            })),
        ),
        entry(
            6,
            process(ProcessEventKind::Exited(ProcessExit {
                outcome: ExitOutcome::Code(1),
                duration_ms: 1200,
                at: Timestamp(0),
            })),
        ),
        entry(
            7,
            Payload::preamble("You are m.", " Be brief.", Default::default(), None),
        ),
        entry(8, Payload::result_verdict(5, WireState::Raw)),
        entry(9, Payload::Resumed(Resumed::new(60_000))),
        entry(10, Payload::archived(None)),
    ];
    assert_eq!(
        render(&log),
        "      1 assistant (record-only, never projected: the model finalized with neither \
         prose nor a call)\n\
         \x20     2 summary   consolidated through #1: the opening\n\
         \x20     3 spot      watch: outside the scope (probed `cargo`)\n\
         \x20     4 images    withheld from here on: the endpoint refused them (image input is \
         not supported)\n\
         \x20     5 process   p#2 spawned: cargo watch\n\
         \x20     6 process   p#2 exited Code(1) after 1200 ms\n\
         \x20     7 preamble  20 B of system prompt\n\
         \x20     8 result_verdict triage resolved result #5\n\
         \x20     9 resumed   ⟢ session resumed after 1m: the host process restarted.\n\
         \x20    10 archived  archived on purpose\n"
    );
}
