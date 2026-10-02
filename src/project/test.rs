use super::*;
use crate::context::Item;
use crate::log::{
    AssistantOutput, CallId, ContentBlock, Disposition, Distilled, Keep, LogEntry, Mark, MarkKind,
    Notification, Origin, Payload, RawResult, Timestamp, ToolCall, Triage, WireState,
};
use serde_json::json;

fn entry(seq: u64, turn: u32, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn,
        at: Timestamp(1000 + seq),
        submission_id: None,
        source: None,
        payload,
    }
}

/// A small log exercising the three core payloads: a user turn, an assistant
/// turn with prose + a tool call, and that call's raw result.
fn sample_log() -> Vec<LogEntry> {
    vec![
        entry(
            0,
            0,
            Payload::User(vec![ContentBlock::Text("hello".into())]),
        ),
        entry(
            1,
            1,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![ContentBlock::Text("let me check".into())],
                    // Insertion order == sorted order, so the expected-bytes
                    // assertion is robust whether serde_json is BTreeMap- or
                    // (under `preserve_order`) IndexMap-backed.
                    vec![ToolCall::new(
                        "bash",
                        "toolu_1",
                        json!({ "command": "ls", "timeout": 5 }),
                    )],
                ),
                upstream_failure: None,
            },
        ),
        entry(
            2,
            1,
            Payload::tool_result("toolu_1", RawResult::text("file.txt"), WireState::Raw),
        ),
    ]
}

/// `serde_json::Map` must be a `BTreeMap` — the sorted key order every other
/// determinism guarantee here is built on (docs/log-and-projection.md). Tool-call
/// `input` is a `Value` *for* this: a map that preserved insertion order would take
/// its order from whatever built it, and anything built from a `HashMap` would
/// randomize per process and silently move the cached prefix.
///
/// This is a property of a **cargo feature**, not of this crate: serde_json's
/// `preserve_order` swaps in an `IndexMap`, and cargo unifies features across the
/// whole runtime graph, so any dependency turning it on flips the behavior here with
/// no compile error. A copy of serde_json with `indexmap` is already in this build's
/// *build-dependency* graph (resolved separately, so it does not reach the binary) —
/// the flip is one normal dependency away. Nothing else would catch it: both
/// projection pins would keep passing, because a single process is self-consistent.
#[test]
fn json_object_keys_serialize_in_sorted_order() {
    let out_of_order = serde_json::json!({ "zeta": 1, "alpha": 2, "mu": 3 });
    assert_eq!(
        serde_json::to_string(&out_of_order).unwrap(),
        r#"{"alpha":2,"mu":3,"zeta":1}"#,
        "serde_json is preserving insertion order — a dependency has enabled the \
         `preserve_order` feature, and the cache-prefix invariant no longer holds"
    );
}

#[test]
fn projection_is_byte_stable() {
    let log = sample_log();
    let a = serde_json::to_string(&project(&log)).unwrap();
    let b = serde_json::to_string(&project(&log)).unwrap();
    assert_eq!(
        a, b,
        "re-projecting an identical log must yield identical bytes"
    );
}

/// Pins the exact wire encoding. If a field is reordered, renamed, or
/// added, this fails — which is the point: such a change silently breaks the
/// cache prefix in production, so we want it to break a test first.
///
/// Each tool result is prefixed with [seq] on the wire so results are
/// describable as 'reread 14' / 'branch from 14' — the entry's stable seq.
#[test]
fn projection_matches_expected_wire_bytes() {
    let wire = serde_json::to_string(&project(&sample_log())).unwrap();
    let expected = r#"[{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","content":[{"type":"text","text":"let me check"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls","timeout":5}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"[2] file.txt"}]}]"#;
    assert_eq!(wire, expected);
}

/// Pins the sourced case: a `User` entry carrying a `source` opens its wire
/// block with the `source:` prefix (docs/control-api.md), and that is the only
/// byte difference from the unsourced pin above — every other block is
/// identical. The prefix is what lets the model tell voices apart when more
/// than one client submits into a session.
#[test]
fn projection_with_source_matches_expected_wire_bytes() {
    let mut log = sample_log();
    log[0].source = Some("mcp[claude-code]".into());
    let wire = serde_json::to_string(&project(&log)).unwrap();
    let expected = r#"[{"role":"user","content":[{"type":"text","text":"mcp[claude-code]: hello"}]},{"role":"assistant","content":[{"type":"text","text":"let me check"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls","timeout":5}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"[2] file.txt"}]}]"#;
    assert_eq!(wire, expected);
}

/// A sourced turn and its unsourced twin differ on the wire only by the
/// prefix line, so a log with no source projects byte-for-byte as it did
/// before the field existed.
#[test]
fn source_does_not_move_unsourced_bytes() {
    let unsourced = serde_json::to_string(&project(&sample_log())).unwrap();
    let mut sourced = sample_log();
    sourced[0].source = Some("web".into());
    let sourced = serde_json::to_string(&project(&sourced)).unwrap();
    assert_ne!(unsourced, sourced);
    assert!(sourced.contains(r#""text":"web: hello""#));
}

/// An attachment a turn carried projects as its notice — what arrived and
/// where — in its own text block after the turn's prose, never as its bytes
/// (docs/inbox.md). Pins the log shape too: the block round-trips through the
/// jsonl, so a renamed field here would silently drop attachments from old logs.
#[test]
fn attachment_projects_as_its_notice() {
    let arrival = crate::attachments::Arrival {
        name: "shot.png".into(),
        path: ".efference/s1/attachments/composer/shot.png".into(),
        kind: crate::attachments::Kind::Image {
            format: "PNG".into(),
            width: 1280,
            height: 800,
            bytes: 245_760,
        },
    };
    let payload = Payload::User(vec![
        ContentBlock::Text("why is this red".into()),
        ContentBlock::Attachment(arrival),
    ]);
    let log = vec![entry(0, 0, payload)];
    let wire = serde_json::to_string(&project(&log)).unwrap();
    let expected = r#"[{"role":"user","content":[{"type":"text","text":"why is this red"},{"type":"text","text":"⟢ attached: shot.png — PNG image, 1280×800, 240.0 KB — at .efference/s1/attachments/composer/shot.png"}]}]"#;
    assert_eq!(wire, expected);
    let json = serde_json::to_string(&log[0].payload).unwrap();
    assert_eq!(
        json,
        r#"{"User":[{"Text":"why is this red"},{"Attachment":{"name":"shot.png","path":".efference/s1/attachments/composer/shot.png","kind":{"Image":{"format":"PNG","width":1280,"height":800,"bytes":245760}}}}]}"#
    );
}

/// A sourced turn carrying only attachments has no prose for its `source:` to open, so the
/// source takes a line of its own and the harness's `⟢` note still starts a line.
#[test]
fn a_sourced_attachment_only_turn_keeps_the_sigil_at_a_line_start() {
    let arrival = crate::attachments::Arrival {
        name: "a.txt".into(),
        path: "att/composer/a.txt".into(),
        kind: crate::attachments::Kind::Binary { bytes: 3 },
    };
    let mut log = vec![entry(
        0,
        0,
        Payload::User(vec![ContentBlock::Attachment(arrival)]),
    )];
    log[0].source = Some("web".into());
    let wire = serde_json::to_string(&project(&log)).unwrap();
    assert!(
        wire.contains(r#""text":"web:\n⟢ attached: a.txt — binary, 3 B — at att/composer/a.txt""#),
        "{wire}"
    );
}

/// A log whose tool result carries one image, in the given wire state.
fn image_log(wire: WireState) -> Vec<LogEntry> {
    use crate::log::ResultImage;
    let mut log = sample_log();
    if let Payload::ToolResult { raw, wire: w, .. } = &mut log[2].payload {
        raw.content = "1456×819 image/webp, 1560 visual tokens".into();
        raw.images = vec![ResultImage {
            media_type: "image/webp".into(),
            data: "UklGRg==".into(),
            width: 1456,
            height: 819,
        }];
        *w = wire;
    }
    log
}

/// Pins the exact wire encoding of an image-bearing tool result: `content`
/// takes the block-array form — the `[seq]`-prefixed text block first, then one
/// `image` block per stored image, `source` spelled
/// `{"type":"base64","media_type":…,"data":…}` with the stored base64 projected
/// verbatim (docs/log-and-projection.md). This is the byte pin the image slice
/// moved deliberately; reorder or rename a field here and production cache
/// breaks silently, so it must break here first.
#[test]
fn image_tool_result_matches_expected_wire_bytes() {
    let wire = serde_json::to_string(&project(&image_log(WireState::Raw))).unwrap();
    let expected = r#"[{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","content":[{"type":"text","text":"let me check"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls","timeout":5}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"[2] 1456×819 image/webp, 1560 visual tokens"},{"type":"image","source":{"type":"base64","media_type":"image/webp","data":"UklGRg=="}}]}]}]"#;
    assert_eq!(wire, expected);
}

/// A log whose tool result carries a multi-line body under the given verdict.
fn distilled_log(keep: Keep, note: Option<&str>) -> Vec<LogEntry> {
    let mut log = sample_log();
    if let Payload::ToolResult { raw, wire, .. } = &mut log[2].payload {
        raw.content = "alpha\nbeta\ngamma\ndelta".into();
        let mut verdict = Distilled::new(keep);
        if let Some(note) = note {
            verdict = verdict.with_note(note);
        }
        *wire = verdict.into();
    }
    log
}

/// The boot identity is metadata, not wire content. A projection change that
/// starts emitting it or changing the messages for it must fail here first.
#[test]
fn boot_inference_identity_does_not_project() {
    let identity = crate::log::LoggedInference::new(
        "router",
        crate::api::Provider::OpenRouter,
        "google/gemini-3.7-flash",
    );
    let mut with_identity = sample_log();
    with_identity.insert(
        0,
        entry(
            0,
            0,
            Payload::Preamble {
                system: "sys".into(),
                context: Default::default(),
                inference: Some(identity),
            },
        ),
    );
    assert_eq!(
        serde_json::to_string(&project(&with_identity)).unwrap(),
        serde_json::to_string(&project(&sample_log())).unwrap(),
        "boot inference identity is metadata, not wire content"
    );
}

/// Pins the exact wire bytes of a **distilled** tool result — the shape most
/// results take once their gate has run, and the one the other pins miss: the
/// shared fixture's only result is `WireState::Raw`, so a render change to the
/// distilled shape would otherwise pass every byte pin.
///
/// Three properties are fixed here. A partial verdict carries `@@ lines a-b @@` per
/// span. A verdict spanning the whole result carries **no header** — the marker exists
/// to show where curation cut, and one that spans everything states only "all of it"
/// (docs/redaction-fork.md). A note renders as a trailing `⟢` breadcrumb.
///
/// Distilled results sit in `messages`, ahead of the cache breakpoint, so a change here
/// moves the production prefix silently. It must break this test first.
#[test]
fn distilled_tool_result_matches_expected_wire_bytes() {
    let partial = serde_json::to_string(&project(&distilled_log(
        Keep::Ranges(vec![(2, 3)]),
        Some("the middle mattered"),
    )))
    .unwrap();
    assert_eq!(
        partial,
        r#"[{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","content":[{"type":"text","text":"let me check"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls","timeout":5}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"[2] @@ lines 2-3 @@\nbeta\ngamma\n⟢ the middle mattered"}]}]"#
    );

    let whole = serde_json::to_string(&project(&distilled_log(Keep::All, None))).unwrap();
    assert_eq!(
        whole,
        r#"[{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","content":[{"type":"text","text":"let me check"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls","timeout":5}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"[2] alpha\nbeta\ngamma\ndelta"}]}]"#
    );

    let nothing =
        serde_json::to_string(&project(&distilled_log(Keep::None, Some("no signal")))).unwrap();
    assert_eq!(
        nothing,
        r#"[{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","content":[{"type":"text","text":"let me check"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls","timeout":5}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"[2] ⟢ no signal"}]}]"#
    );
}

/// Pins the `FollowingUserBlock` placement's exact wire bytes: the
/// `tool_result` keeps the bare-string content form (byte-identical to a
/// text-only result), and each image trails it as a top-level `image` block in
/// the same user message — the portable shape for bridged providers
/// (docs/log-and-projection.md).
#[test]
fn following_user_block_matches_expected_wire_bytes() {
    let wire = serde_json::to_string(&project_with(
        &image_log(WireState::Raw),
        ImagePlacement::FollowingUserBlock,
    ))
    .unwrap();
    let expected = r#"[{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","content":[{"type":"text","text":"let me check"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls","timeout":5}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"[2] 1456×819 image/webp, 1560 visual tokens"},{"type":"image","source":{"type":"base64","media_type":"image/webp","data":"UklGRg=="}}]}]"#;
    assert_eq!(wire, expected);
}

/// Elision and gate failure shed the trailing image block in the
/// `FollowingUserBlock` placement too, and a text-only log projects
/// byte-identically under either placement.
#[test]
fn following_user_block_sheds_on_elision_and_is_identity_for_text() {
    let has_image = |state: WireState| {
        serde_json::to_string(&project_with(
            &image_log(state),
            ImagePlacement::FollowingUserBlock,
        ))
        .unwrap()
        .contains(r#""type":"image""#)
    };
    assert!(has_image(WireState::Raw));
    assert!(!has_image(WireState::Elided));
    assert!(!has_image(WireState::TriageFailed {
        why: "peek died".into(),
    }));
    let log = sample_log();
    assert_eq!(
        serde_json::to_string(&project(&log)).unwrap(),
        serde_json::to_string(&project_with(&log, ImagePlacement::FollowingUserBlock)).unwrap(),
    );
}

/// Pins the `AfterInterposedTurn` placement's exact wire bytes: the `tool_result`
/// keeps the bare-string form, then an assistant turn naming the result's `seq`
/// handle, then the images in their own user message. Gemini rejects a replayed
/// thought signature when an image sits between the signed `tool_use` and the next
/// assistant turn, and this is the only shape that clears it
/// (docs/log-and-projection.md).
#[test]
fn after_interposed_turn_matches_expected_wire_bytes() {
    let wire = serde_json::to_string(&project_with(
        &image_log(WireState::Raw),
        ImagePlacement::AfterInterposedTurn,
    ))
    .unwrap();
    let expected = r#"[{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","content":[{"type":"text","text":"let me check"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls","timeout":5}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"[2] 1456×819 image/webp, 1560 visual tokens"}]},{"role":"assistant","content":[{"type":"text","text":"…receiving the image from [2]…"}]},{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/webp","data":"UklGRg=="}}]}]"#;
    assert_eq!(wire, expected);
}

/// The interposed turn exists only when images survive to the wire: a state that
/// sheds them (elision, a failed gate) leaves no fabricated assistant turn behind,
/// and a text-only log projects byte-identically under this placement too — so
/// sessions that never pull an image see the same bytes as before the arm existed.
#[test]
fn after_interposed_turn_is_absent_without_images() {
    let projected = |state: WireState| {
        serde_json::to_string(&project_with(
            &image_log(state),
            ImagePlacement::AfterInterposedTurn,
        ))
        .unwrap()
    };
    assert!(projected(WireState::Raw).contains("…receiving the image from [2]…"));
    for shed in [
        WireState::Elided,
        WireState::TriageFailed {
            why: "peek died".into(),
        },
    ] {
        let wire = projected(shed);
        assert!(!wire.contains(r#""type":"image""#));
        assert!(!wire.contains("…receiving"));
    }
    let log = sample_log();
    assert_eq!(
        serde_json::to_string(&project(&log)).unwrap(),
        serde_json::to_string(&project_with(&log, ImagePlacement::AfterInterposedTurn)).unwrap(),
    );
}

/// Role alternation holds under `AfterInterposedTurn`: the projected wire never
/// puts two same-role messages back to back. A bare second user message is one of
/// the shapes Gemini rejects, so this is the invariant the interposed turn buys.
#[test]
fn after_interposed_turn_keeps_roles_alternating() {
    let wire = project_with(
        &image_log(WireState::Raw),
        ImagePlacement::AfterInterposedTurn,
    );
    assert!(
        wire.windows(2).all(|pair| pair[0].role != pair[1].role),
        "consecutive same-role messages: {:?}",
        wire.iter().map(|m| m.role).collect::<Vec<_>>()
    );
}

/// Images ride `Raw` and `Distilled` (a distill verdict addresses text lines,
/// not the image the model pulled on purpose) and are shed by `Elided` and
/// `TriageFailed` — whose results project in the bare-string form, byte-identical
/// to a text-only result in the same state.
#[test]
fn image_survives_distill_and_is_shed_by_elision() {
    let keeps = |state: WireState| {
        serde_json::to_string(&project(&image_log(state)))
            .unwrap()
            .contains(r#""type":"image""#)
    };
    assert!(keeps(WireState::Raw));
    assert!(keeps(WireState::from(
        Distilled::new(Keep::Ranges(vec![(1, 1)])).with_note("kept the header")
    )));
    assert!(!keeps(WireState::Elided));
    assert!(!keeps(WireState::TriageFailed {
        why: "peek died".into(),
    }));
}

/// A `RawResult` without images serializes byte-identically to before the
/// `images` field existed, and a pre-images log line still deserializes — the
/// additive-schema guarantee that keeps every existing log readable and its
/// projection unmoved.
#[test]
fn imageless_raw_result_serialization_is_unchanged() {
    let raw = RawResult::text("file.txt");
    assert_eq!(
        serde_json::to_string(&raw).unwrap(),
        r#"{"content":"file.txt","is_error":false}"#
    );
    let old: RawResult =
        serde_json::from_str(r#"{"content":"file.txt","is_error":false}"#).unwrap();
    assert!(old.images.is_empty());
}

/// A record-only partial (docs/transport.md) projects to **nothing**: appending one
/// leaves the wire byte-identical to the log without it. Both properties this unit
/// rests on live here — the retry that follows an aborted stream re-sends the same
/// bytes (riding the cached prefix), and a thinking-only partial cannot project as a
/// dangling thinking block.
#[test]
fn record_only_partial_projects_to_nothing() {
    let clean = serde_json::to_string(&project(&sample_log())).unwrap();
    let mut log = sample_log();
    log.push(entry(
        3,
        2,
        Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(
                vec![ContentBlock::Thinking {
                    text: "eleven thousand characters of thinking".into(),
                    signature: "".into(),
                }],
                vec![],
                vec![],
            ),
            upstream_failure: Some("stream ended before finalizing (no message_stop)".into()),
        },
    ));
    // An empty one too — the durable record of a pre-stream request failure.
    log.push(entry(
        4,
        3,
        Payload::Assistant {
            output: AssistantOutput::Messages(vec![]),
            upstream_failure: Some("upstream API returned 429: rate limited".into()),
        },
    ));
    let with_partials = serde_json::to_string(&project(&log)).unwrap();
    assert_eq!(clean, with_partials);
}

/// A failed HTTP attempt the retry handler absorbed gets the same treatment
/// (docs/transport.md): the loop drains the body out of the client's queue, appends
/// it as a record-only entry, and the wire is byte-identical to a log without it —
/// so recording a 429 that recovered cannot move the cached prefix. Round-trips the
/// entry through the log's own serialization first, since the record's whole point
/// is being readable back out of the jsonl.
#[test]
fn an_absorbed_attempt_record_round_trips_and_projects_to_nothing() {
    let clean = serde_json::to_string(&project(&sample_log())).unwrap();

    let failure = crate::api::UpstreamAttemptFailure {
        status: 429,
        body: r#"{"error":{"message":"rate limited"}}"#.into(),
    };
    let appended = entry(
        3,
        2,
        Payload::Assistant {
            output: AssistantOutput::Messages(vec![]),
            upstream_failure: Some(failure.record_text().into()),
        },
    );

    let line = serde_json::to_string(&appended).unwrap();
    let read_back: crate::log::LogEntry = serde_json::from_str(&line).unwrap();
    match &read_back.payload {
        Payload::Assistant {
            upstream_failure: Some(why),
            ..
        } => {
            assert!(why.why().contains("HTTP 429"), "{why}");
            assert!(why.why().contains("rate limited"), "{why}");
        }
        other => panic!("expected a record-only assistant entry, got {other:?}"),
    }

    let mut log = sample_log();
    log.push(read_back);
    assert_eq!(clean, serde_json::to_string(&project(&log)).unwrap());
}

/// Pins the exact wire encoding of a thinking-bearing assistant turn
/// (docs/transport.md): thinking leads (before text, before `tool_use`), the
/// `signature` rides verbatim, and `redacted_thinking` renders its opaque `data`. This is
/// the byte pin the thinking slice moves — reorder/rename a thinking field and it fails
/// here first, before it can silently break the cache.
#[test]
fn thinking_turn_matches_expected_wire_bytes() {
    let log = vec![
        entry(0, 1, Payload::User(vec![ContentBlock::Text("go".into())])),
        entry(
            1,
            1,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![
                        ContentBlock::Thinking {
                            text: "let me weigh it".into(),
                            signature: "SiG==".into(),
                        },
                        ContentBlock::RedactedThinking {
                            data: "ENC==".into(),
                        },
                    ],
                    vec![ContentBlock::Text("running it".into())],
                    vec![ToolCall::new("bash", "toolu_1", json!({ "command": "ls" }))],
                ),
                upstream_failure: None,
            },
        ),
    ];
    let wire = serde_json::to_string(&project(&log)).unwrap();
    let expected = r#"[{"role":"user","content":[{"type":"text","text":"go"}]},{"role":"assistant","content":[{"type":"thinking","thinking":"let me weigh it","signature":"SiG=="},{"type":"redacted_thinking","data":"ENC=="},{"type":"text","text":"running it"},{"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls"}}]}]"#;
    assert_eq!(wire, expected);
}

/// A `ConfigEdit` is audit-only **in every lifecycle state**: appending a `Proposed`,
/// `Committed`, or `Cancelled` entry anywhere in the log must not change a single projected
/// byte (its effect reaches the model through the classifier, not the wire). If this ever
/// fails, a config edit has leaked onto the cached prefix (docs/log-and-projection.md).
#[test]
fn config_edit_projects_to_nothing() {
    use crate::log::{ConfigChange, ConfigEdit, EditState};
    let without = serde_json::to_string(&project(&sample_log())).unwrap();

    for state in [
        EditState::Proposed,
        EditState::Committed,
        EditState::Cancelled,
    ] {
        let mut with = sample_log();
        with.push(entry(
            3,
            1,
            Payload::ConfigEdit(ConfigEdit {
                state,
                resolves: None,
                change: ConfigChange::Command {
                    command: "cargo nextest".into(),
                },
                note: "promoted: standing-safe in scope".into(),
            }),
        ));
        let with = serde_json::to_string(&project(&with)).unwrap();
        assert_eq!(
            without, with,
            "a {state:?} config edit must not perturb the projected wire"
        );
    }
}

/// Per-call tool telemetry (docs/tools.md) is log metadata in the same sense
/// timestamps are: a log carrying one entry per tool call projects
/// **byte-identically** to the same log without them. This is the pin that lets
/// the record be written unconditionally on every call — the moment it could
/// move a byte, it would be moving the cached prefix of every session that runs
/// a tool (docs/log-and-projection.md). Same shape as the `Triage.thinking` pin.
#[test]
fn tool_telemetry_projects_to_identical_bytes() {
    use crate::log::{BashTelemetry, EditTelemetry, ToolOutcome, ToolTelemetry};
    let without = serde_json::to_string(&project(&sample_log())).unwrap();
    // Interleaved as the loop appends it (after the result), and again at the
    // tail — position must not matter either.
    for at in [2, 3] {
        let mut with = sample_log();
        with.insert(
            at,
            entry(
                99,
                1,
                Payload::ToolTelemetry(ToolTelemetry {
                    call_id: CallId("toolu_1".into()),
                    tool: "bash".into(),
                    duration_ms: 1234,
                    outcome: ToolOutcome::Rejected,
                    result_bytes: 8192,
                    images: 2,
                    bash: Some(BashTelemetry {
                        exit_status: Some("1".into()),
                        background: false,
                        handle: Some("p#1".into()),
                    }),
                    edit: Some(EditTelemetry {
                        operation: Some("replace".into()),
                        anchor_bytes: Some(40),
                        content_bytes: Some(900),
                    }),
                }),
            ),
        );
        assert_eq!(
            without,
            serde_json::to_string(&project(&with)).unwrap(),
            "telemetry appended at index {at} must not perturb the projected wire"
        );
    }
}

/// A process lifecycle record (docs/supervisor.md) is log metadata too: the mint
/// and the exit are appended mid-session, so if either moved a projected byte it
/// would invalidate the cached prefix of every session that backgrounds a command
/// (docs/log-and-projection.md).
#[test]
fn process_events_project_to_identical_bytes() {
    use crate::log::{ExitOutcome, ProcessEvent, ProcessEventKind, ProcessExit, ProcessSpawn};
    let without = serde_json::to_string(&project(&sample_log())).unwrap();
    let events = [
        ProcessEventKind::Spawned(ProcessSpawn {
            command: "cargo watch -x test".into(),
            call_id: Some(CallId("toolu_1".into())),
            pid: Some(4242),
            at: Timestamp(1_754_300_000_000),
        }),
        ProcessEventKind::Exited(ProcessExit {
            outcome: ExitOutcome::Code(1),
            duration_ms: 12_000,
            at: Timestamp(1_754_300_012_000),
        }),
    ];
    // Interleaved mid-log and at the tail — position must not matter either.
    for at in [2, 3] {
        for kind in events.clone() {
            let mut with = sample_log();
            with.insert(
                at,
                entry(99, 1, Payload::ProcessEvent(ProcessEvent { id: 3, kind })),
            );
            assert_eq!(
                without,
                serde_json::to_string(&project(&with)).unwrap(),
                "a process record appended at index {at} must not perturb the projected wire"
            );
        }
    }
}

/// The boot preamble snapshot is replay/audit only: appending one anywhere in the log
/// must not change a single projected byte — the system prompt is carried by the
/// request's `system` field, never the `messages` array (docs/context.md).
#[test]
fn preamble_snapshot_projects_to_nothing() {
    let without = serde_json::to_string(&project(&sample_log())).unwrap();
    let mut with = sample_log();
    with.push(entry(
        3,
        1,
        Payload::Preamble {
            system: "You are an agent.\n\n<CLAUDE>\nthe whole repo map\n</CLAUDE>".into(),
            context: Default::default(),
            inference: None,
        },
    ));
    let with = serde_json::to_string(&project(&with)).unwrap();
    assert_eq!(
        without, with,
        "a preamble snapshot must not perturb the projected wire"
    );
}

mod cache;
mod coalesce;
mod consolidate;
mod dialect;
mod distilled;
mod notifications;

/// A `Resumed` entry projects one harness text block at its position and leaves
/// every earlier turn's bytes unchanged (docs/log-and-projection.md).
#[test]
fn resumed_projects_a_notice_after_the_prefix() {
    let before = serde_json::to_string(&project(&sample_log())).unwrap();
    let mut log = sample_log();
    log.push(entry(
        3,
        2,
        Payload::Resumed(crate::log::Resumed {
            gap_ms: 90_000,
            lost_processes: vec!["p#1".into()],
            dropped_calls: vec![CallId::from("toolu_2")],
        }),
    ));
    let after = serde_json::to_string(&project(&log)).unwrap();
    let prefix = before.trim_end_matches(']');
    assert!(after.starts_with(prefix), "{after}");
    assert!(after.contains("session resumed after 1m"), "{after}");
    assert!(
        after.contains("p#1") && after.contains("toolu_2"),
        "{after}"
    );
}
