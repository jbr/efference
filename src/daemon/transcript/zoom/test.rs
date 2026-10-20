use super::super::fixtures::*;
use super::*;
use crate::frontend::Approval;
use crate::log::{Keep, Origin, Triaged};
use crate::spotter::jev::{Axes, Precedent};

fn child<'a>(me: Option<&'a str>) -> Reading<'a> {
    Reading {
        session: Some("child"),
        me,
        held: None,
    }
}

fn handle(text: &str) -> Handle {
    Handle::parse(text).unwrap()
}

/// A gated bash call the reading parent allowed, whose result the session saw part of:
/// the exact input with the intent apart, the spotter's verdict in plain words, the
/// decision, the result's seq and size, the lines the session saw, and the raw body.
#[test]
fn a_gated_call_shows_its_decision_trail_and_raw_result() {
    let mut log = Fixture::new();
    log.add(turn(
        "Checking the site.",
        vec![
            ToolCall::new("read", "toolu_0", serde_json::json!({ "paths": ["a.rs"] })),
            bash(
                "toolu_1",
                "curl -sI https://example.com",
                "fetch the headers",
            ),
        ],
    ));
    log.add(Payload::Spot(crate::log::Spot {
        call_id: "toolu_1".into(),
        stance: Stance::Gate,
        reason: "level 2 (scope fit, 21%): needs 1 approved call like it, found none.".into(),
        origin: Origin::Model,
        model: None,
        probed_prefix: None,
        axes: Some(Axes {
            in_scope: 0.21,
            precedent: Some(Precedent {
                candidates: 0,
                same: 0,
                wider: 0,
                required: Some(1),
                compared: Vec::new(),
            }),
            ..Axes::default()
        }),
    }));
    log.add(decision(
        "toolu_1",
        Approval::Approve,
        Some("session[parent]"),
    ));
    log.add(Payload::tool_result(
        "toolu_1",
        RawResult::text("HTTP/2 200\ncontent-type: text/html\nserver: ECS\n"),
        Triaged::new(Keep::Ranges(vec![(1, 1)]))
            .with_note("the status line")
            .into(),
    ));
    let expected = "transcript of child, seq 0.2\n\
         call 0.2: bash\n\
         intent: fetch the headers\n\
         input: {\n  \"command\": \"curl -sI https://example.com\"\n}\n\
         reviewer: held it for a decision\n\
         held because: its reviewer is unsure this fits the session's scope; no earlier call \
         like it was allowed\n\
         decision: allowed by you (seq 2)\n\
         result: seq 3, ok, 3 lines, 47 B\n\
         it saw: line 1 of 3: the status line\n\
         raw:\n\
         HTTP/2 200\ncontent-type: text/html\nserver: ECS\n";
    assert_eq!(
        zoom(log.entries(), handle("0.2"), &child(Some("parent"))).unwrap(),
        expected
    );
    // The result's own seq zooms into the same call.
    assert_eq!(
        zoom(log.entries(), handle("3"), &child(Some("parent"))).unwrap(),
        expected.replace("seq 0.2\n", "seq 3\n")
    );
}

/// A body past the inline cap is cut on a char boundary, with how much was cut; the
/// operator's reader (no `me`) is told the route that serves the rest and each image, a
/// named reader (an agent, an MCP driver), which cannot call it, only the size and the
/// image.
#[test]
fn a_long_result_is_capped_and_its_images_are_addressed() {
    let mut log = Fixture::new();
    log.add(turn("", vec![bash("toolu_1", "cat big", "read it")]));
    let body = "é".repeat(10_000);
    log.add(Payload::tool_result(
        "toolu_1",
        RawResult::text(body.clone()).with_images(vec![crate::log::ResultImage {
            media_type: "image/png".into(),
            data: "AAAA".into(),
            width: 800,
            height: 600,
        }]),
        WireState::Raw,
    ));
    let text = zoom(log.entries(), handle("0"), &child(None)).unwrap();
    let kept = crate::daemon::entry_view::MAX_INLINE_RESULT_BYTES;
    assert!(
        text.contains(&format!("raw:\n{}\n", &body[..kept])),
        "{text}"
    );
    assert!(
        text.contains("cut: 3.5 KB more of 19.5 KB; GET /log/1 serves the whole result\n"),
        "{text}"
    );
    assert!(
        text.ends_with("image 0: image/png 800×600, at GET /log/1/images/0\n"),
        "{text}"
    );
    assert!(text.contains("reviewer: not asked\n"), "{text}");
    assert!(
        text.contains("decision: none; the gate did not hold it\n"),
        "{text}"
    );

    let named = zoom(log.entries(), handle("0"), &child(Some("parent"))).unwrap();
    assert!(named.contains("cut: 3.5 KB more of 19.5 KB\n"), "{named}");
    assert!(named.ends_with("image 0: image/png 800×600\n"), "{named}");
    assert!(!named.contains("/log/"), "{named}");
}

/// A prose entry is its whole text under its sender; a turn by its bare seq lists the
/// calls it issued when there are several.
#[test]
fn a_prose_entry_is_whole() {
    let mut log = Fixture::new();
    log.user("first line\n\nsecond paragraph", Some("session[parent]"));
    log.add(turn(
        "Two things to check.",
        vec![
            bash("toolu_1", "ls", "look"),
            bash("toolu_2", "pwd", "where"),
        ],
    ));
    assert_eq!(
        zoom(log.entries(), handle("0"), &child(Some("parent"))).unwrap(),
        "transcript of child, seq 0\nyou:\nfirst line\n\nsecond paragraph\n"
    );
    assert_eq!(
        zoom(log.entries(), handle("1"), &child(None)).unwrap(),
        "transcript of child, seq 1\n\
         child:\n\
         Two things to check.\n\
         1.1  bash `ls`\n\
         1.2  bash `pwd`\n\
         read each call in full by its handle: 1.1 and 1.2\n"
    );
}

/// A seq past the log, a call index past the entry's calls, and a malformed handle
/// each say what is wrong and what would work.
#[test]
fn a_missing_entry_is_a_clear_error() {
    let mut log = Fixture::new();
    log.user("hi", None);
    log.add(turn(
        "",
        vec![
            bash("toolu_1", "ls", "look"),
            bash("toolu_2", "pwd", "where"),
        ],
    ));
    assert_eq!(
        zoom(log.entries(), handle("9"), &child(None)).unwrap_err(),
        "no entry at seq 9: the log runs from seq 0 to 1"
    );
    assert_eq!(
        zoom(log.entries(), handle("1.3"), &child(None)).unwrap_err(),
        "seq 1 issued 2 calls: 1.1 and 1.2"
    );
    assert_eq!(
        zoom(log.entries(), handle("0.1"), &child(None)).unwrap_err(),
        "seq 0 is not an assistant turn; seq.n names one of the calls a turn issued"
    );
    assert_eq!(
        Handle::parse("8.x").unwrap_err(),
        "seq must be an entry's seq or a call's seq.n, like 8 or 8.2; got \"8.x\""
    );
    assert_eq!(
        Handle::parse("8.2").unwrap(),
        Handle {
            seq: 8,
            call: Some(2)
        }
    );
}

/// An escalated call's decision trail names each escalation, with the seq recording it,
/// before the decision; the escalation's own entry zooms to a line naming the call.
#[test]
fn an_escalation_is_in_the_decision_trail() {
    let mut log = Fixture::new();
    log.add(turn(
        "",
        vec![bash("toolu_1", "curl -sI x", "fetch headers")],
    ));
    log.add(escalation(
        "toolu_1",
        Some("session[parent]"),
        Some("grand"),
        Some("is example.com in scope?"),
    ));
    let reading = Reading {
        held: Some("toolu_1"),
        ..child(Some("grand"))
    };
    let text = zoom(log.entries(), handle("0"), &reading).unwrap();
    assert!(
        text.contains(
            "escalated by parent to you: \"is example.com in scope?\" (seq 1)\ndecision: pending"
        ),
        "{text}"
    );
    assert_eq!(
        zoom(log.entries(), handle("1"), &reading).unwrap(),
        "transcript of child, seq 1\nescalation of 0: escalated by parent to you: \"is \
         example.com in scope?\"\n"
    );
}
