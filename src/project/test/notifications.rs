//! Notification and surfaced-context projection at the triage position (docs/inbox.md).

use super::*;

fn notification(source: &str, body: &str) -> Payload {
    Payload::Ambient(Notification {
        source: source.into(),
        body: body.into(),
        slot: None,
    })
}

/// A pinned notification projects as a `⟢ source: body` user block **at its
/// `Triage`'s position** (docs/inbox.md) — body looked up from the `Ambient` arrival;
/// the dismissed one never projects, and the arrivals themselves project to
/// nothing.
#[test]
fn pinned_notification_projects_at_triage_position() {
    let log = vec![
        entry(
            0,
            0,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![ToolCall::new("bash", "t", json!({}))],
                ),
                upstream_failure: None,
            },
        ),
        entry(1, 0, notification("test-watch", "3 failures")), // pinned
        entry(2, 0, notification("reviewer", "a nit")),        // dismissed
        entry(
            3,
            0,
            Payload::tool_result("t", RawResult::text("out"), WireState::Raw),
        ),
        entry(
            4,
            0,
            Payload::Triage(Triage {
                for_result: Some(3),
                origin: Origin::Model,
                raw_keep: "all".into(),
                inbox: vec![(1, Disposition::Pin), (2, Disposition::Dismiss)],
                surfaced: Vec::new(),
                retries: Vec::new(),
                thinking: Vec::new(),
            }),
        ),
    ];
    let messages = project(&log);
    // assistant(tool_use) · user(tool_result) · user(pinned notification).
    assert_eq!(messages.len(), 3, "{messages:#?}");
    assert_eq!(messages[0].role, Role::Assistant);
    assert_eq!(messages[1].role, Role::User); // the tool_result, flushed first
    assert_eq!(messages[2].role, Role::User); // the pin block, at the Triage position
    assert!(
        matches!(&messages[2].content[0], ApiBlock::Text { text, .. } if text == "⟢ test-watch: 3 failures")
    );
    let wire = serde_json::to_string(&messages).unwrap();
    assert!(
        !wire.contains("reviewer") && !wire.contains("a nit"),
        "dismissed off wire: {wire}"
    );
}

/// A surfaced context item projects as a `⟢ context · …` read-me reminder **at its
/// `Triage`'s position** (docs/triage.md) — the one-line description and
/// read path re-rendered from the boot `Preamble` listing, not stored on the verdict.
/// This is the byte pin the surfacing slice moves: change the render and it fails here
/// first, before it can silently break the cache. A surfaced name absent from the
/// current listing (a cross-boot change) drops silently — deterministic, never a panic.
#[test]
fn surfaced_context_item_projects_at_triage_position() {
    use crate::context::{Resolved, Section};
    let listing = Resolved {
        sections: vec![Section {
            heading: Some("Skills".into()),
            description: None,
            items: vec![Item::Listed {
                name: "ferritin".into(),
                description: "look up Rust docs".into(),
                path: "skills/ferritin.md".into(),
            }],
        }],
    };
    let log = vec![
        entry(
            0,
            0,
            Payload::Preamble {
                system: "sys".into(),
                context: listing,
                inference: None,
            },
        ),
        entry(
            1,
            0,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![ToolCall::new("grep", "t", json!({}))],
                ),
                upstream_failure: None,
            },
        ),
        entry(
            2,
            0,
            Payload::tool_result("t", RawResult::text("no match"), WireState::Raw),
        ),
        entry(
            3,
            0,
            Payload::Triage(Triage {
                for_result: Some(2),
                origin: Origin::Model,
                raw_keep: "all".into(),
                inbox: Vec::new(),
                // "unknown" isn't in the listing → drops; only ferritin renders.
                surfaced: vec!["ferritin".into(), "unknown".into()],
                retries: Vec::new(),
                thinking: Vec::new(),
            }),
        ),
    ];
    let messages = project(&log);
    // assistant(tool_use) · user(tool_result) · user(surfacing reminder). The Preamble
    // projects to nothing, so it adds no message.
    assert_eq!(messages.len(), 3, "{messages:#?}");
    assert_eq!(messages[2].role, Role::User); // the reminder, at the Triage position
    assert!(
        matches!(&messages[2].content[0], ApiBlock::Text { text, .. }
                if text == "⟢ context · ferritin may be relevant: look up Rust docs — read skills/ferritin.md"),
        "{:#?}",
        messages[2].content[0]
    );
}

/// An `Ambient` arrival with no resolving verdict (left / un-pinned) is audit-only
/// — it stays off the canonical wire.
#[test]
fn unpinned_ambient_projects_to_nothing() {
    let log = vec![
        entry(0, 0, Payload::User(vec![ContentBlock::Text("hi".into())])),
        entry(1, 0, notification("src", "left notice")),
    ];
    let messages = project(&log);
    assert_eq!(messages.len(), 1, "only the user message: {messages:#?}");
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("left notice")
    );
}

/// The fork's recorded reasoning (`Triage.thinking`, docs/triage.md) is invisible
/// to the wire: a log whose gate recorded thinking projects **byte-identically** to the
/// same log without it. Recording it is an audit move, so it must not be able to move the
/// cached prefix — this is the pin that says so, in the same spirit as the three envelope
/// pins.
#[test]
fn recorded_triage_thinking_projects_to_identical_bytes() {
    let gate = |thinking: Vec<String>| {
        vec![
            entry(
                0,
                0,
                Payload::Assistant {
                    output: AssistantOutput::from_legacy_messages(
                        vec![],
                        vec![],
                        vec![ToolCall::new("bash", "t", json!({}))],
                    ),
                    upstream_failure: None,
                },
            ),
            entry(1, 0, notification("test-watch", "3 failures")),
            entry(
                2,
                0,
                Payload::tool_result("t", RawResult::text("out"), WireState::Raw),
            ),
            entry(
                3,
                0,
                Payload::Triage(Triage {
                    for_result: Some(2),
                    origin: Origin::Model,
                    raw_keep: "all".into(),
                    inbox: vec![(1, Disposition::Pin)],
                    surfaced: Vec::new(),
                    retries: Vec::new(),
                    thinking,
                }),
            ),
        ]
    };
    let without = serde_json::to_string(&project(&gate(Vec::new()))).unwrap();
    let with = serde_json::to_string(&project(&gate(vec![
        "the first 40 lines are the build log; only the failure matters".into(),
    ])))
    .unwrap();
    assert_eq!(
        with, without,
        "recorded fork thinking must not reach the wire"
    );
    assert!(
        !with.contains("the first 40 lines"),
        "the thinking text is audit-only: {with}"
    );
}
