use super::*;
use crate::log::{
    ContentBlock, Disposition, Keep, LogEntry, Mark, MarkKind, Notification, Origin, Payload,
    RawResult, Timestamp, ToolCall, Triage, WireState,
};
use serde_json::json;

fn entry(seq: u64, turn: u32, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn,
        at: Timestamp(1000 + seq),
        submission_id: None,
        payload,
    }
}

/// A small log exercising all three Phase 0 payloads: a user turn, an
/// assistant turn with prose + a tool call, and that call's raw result.
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
                thinking: vec![],
                text: vec![ContentBlock::Text("let me check".into())],
                calls: vec![ToolCall {
                    id: CallId("toolu_1".into()),
                    name: "bash".into(),
                    // Insertion order == sorted order, so the expected-bytes
                    // assertion is robust whether serde_json is BTreeMap- or
                    // (under `preserve_order`) IndexMap-backed.
                    input: json!({ "command": "ls", "timeout": 5 }),
                }],
                upstream_failure: None,
            },
        ),
        entry(
            2,
            1,
            Payload::ToolResult {
                call_id: CallId("toolu_1".into()),
                raw: RawResult {
                    images: Vec::new(),
                    content: "file.txt".into(),
                    is_error: false,
                    content_elided: None,
                },
                wire: WireState::Raw,
            },
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
        *wire = WireState::Distilled {
            keep,
            note: note.map(str::to_string),
        };
    }
    log
}

/// Pins the exact wire bytes of a **distilled** tool result — the shape most results
/// actually take (42 of 44 in one measured session), and which no other pin covered:
/// the fixture's only result is `WireState::Raw`, so a render change to the dominant
/// shape used to pass every byte pin.
///
/// Three properties are fixed here. A partial verdict carries `@@ lines a-b @@` per
/// span. A verdict spanning the whole result carries **no header** — the marker exists
/// to show where curation cut, and one that spans everything states only "all of it"
/// (backlog/distilled-render-overhead.md). A note renders as a trailing `⟢` breadcrumb.
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
    assert!(keeps(WireState::Distilled {
        keep: Keep::Ranges(vec![(1, 1)]),
        note: Some("kept the header".into()),
    }));
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
    let raw = RawResult {
        content: "file.txt".into(),
        is_error: false,
        images: Vec::new(),
        content_elided: None,
    };
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
/// dangling thinking block (the observed failure, 2026-08-21).
#[test]
fn record_only_partial_projects_to_nothing() {
    let clean = serde_json::to_string(&project(&sample_log())).unwrap();
    let mut log = sample_log();
    log.push(entry(
        3,
        2,
        Payload::Assistant {
            thinking: vec![ContentBlock::Thinking {
                text: "eleven thousand characters of thinking".into(),
                signature: "".into(),
            }],
            text: vec![],
            calls: vec![],
            upstream_failure: Some("stream ended before finalizing (no message_stop)".into()),
        },
    ));
    // An empty one too — the durable record of a pre-stream request failure.
    log.push(entry(
        4,
        3,
        Payload::Assistant {
            thinking: vec![],
            text: vec![],
            calls: vec![],
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
            thinking: vec![],
            text: vec![],
            calls: vec![],
            upstream_failure: Some(failure.record_text()),
        },
    );

    let line = serde_json::to_string(&appended).unwrap();
    let read_back: crate::log::LogEntry = serde_json::from_str(&line).unwrap();
    match &read_back.payload {
        Payload::Assistant {
            upstream_failure: Some(why),
            ..
        } => {
            assert!(why.contains("HTTP 429"), "{why}");
            assert!(why.contains("rate limited"), "{why}");
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
                thinking: vec![
                    ContentBlock::Thinking {
                        text: "let me weigh it".into(),
                        signature: "SiG==".into(),
                    },
                    ContentBlock::RedactedThinking {
                        data: "ENC==".into(),
                    },
                ],
                text: vec![ContentBlock::Text("running it".into())],
                calls: vec![ToolCall {
                    id: CallId("toolu_1".into()),
                    name: "bash".into(),
                    input: json!({ "command": "ls" }),
                }],
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
mod distilled;
mod notifications;
