//! The dialect identity guard (docs/transport.md): a turn list is encoded only onto
//! the dialect its assistant turns were recorded in.

use super::*;
use crate::record::log::{ResponsesOutputItem, WireDialect};

fn messages_turn() -> WireTurn {
    WireTurn::Assistant(AssistantOutput::from_legacy_messages(
        vec![],
        vec![ContentBlock::Text("hi".into())],
        vec![],
    ))
}

fn responses_turn() -> WireTurn {
    WireTurn::Assistant(AssistantOutput::Responses(vec![
        ResponsesOutputItem::FunctionCall {
            id: None,
            call_id: CallId("call_1".into()),
            name: "bash".into(),
            arguments: "{}".into(),
            status: None,
        },
    ]))
}

/// A list with no assistant turn belongs to no dialect yet, so it encodes on either:
/// a fresh session is dialect-free until its first reply.
#[test]
fn a_session_without_a_reply_passes_on_either_dialect() {
    let turns = vec![WireTurn::text("hello")];
    assert_eq!(recorded_dialect(&turns), Ok(None));
    assert_eq!(check_dialect(&turns, WireDialect::Messages), Ok(()));
    assert_eq!(check_dialect(&turns, WireDialect::Responses), Ok(()));
    assert_eq!(recorded_dialect(&[]), Ok(None));
}

/// Once a reply is recorded, only its own dialect may encode the list. The guard is
/// about dialect, not the model slug: a recorded session keeps one provider/model
/// identity, while an offline reader may render it under another provider that
/// speaks the same dialect (docs/transport.md).
#[test]
fn a_recorded_dialect_admits_only_itself() {
    let turns = vec![WireTurn::text("hello"), messages_turn()];
    assert_eq!(recorded_dialect(&turns), Ok(Some(WireDialect::Messages)));
    assert_eq!(check_dialect(&turns, WireDialect::Messages), Ok(()));
    assert_eq!(
        check_dialect(&turns, WireDialect::Responses),
        Err(DialectConflict::Foreign {
            recorded: WireDialect::Messages,
            target: WireDialect::Responses,
        })
    );

    let turns = vec![WireTurn::text("hello"), responses_turn()];
    assert_eq!(check_dialect(&turns, WireDialect::Responses), Ok(()));
    let err = check_dialect(&turns, WireDialect::Messages).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("recorded in the Responses dialect"), "{text}");
    assert!(text.contains("speaks Messages"), "{text}");
}

/// Two dialects in one list is a stitched log, not a session; neither target admits it.
#[test]
fn mixed_dialects_are_rejected_on_both_targets() {
    let turns = vec![messages_turn(), WireTurn::text("then"), responses_turn()];
    assert_eq!(recorded_dialect(&turns), Err(DialectConflict::Mixed));
    assert_eq!(
        check_dialect(&turns, WireDialect::Messages),
        Err(DialectConflict::Mixed)
    );
    assert_eq!(
        check_dialect(&turns, WireDialect::Responses),
        Err(DialectConflict::Mixed)
    );
}

/// What the guard stands in front of: each encoder skips the other dialect's
/// assistant turns rather than failing, so without the check a foreign list encodes
/// to a history with every reply missing.
#[test]
fn each_encoder_drops_the_foreign_arm_silently() {
    let turns = vec![WireTurn::text("hello"), responses_turn()];
    let messages = encode_messages(
        &turns,
        ImagePlacement::default(),
        BreakpointPlacement::default(),
    );
    assert_eq!(
        messages.len(),
        1,
        "only the user turn survives: {messages:?}"
    );

    let turns = vec![WireTurn::text("hello"), messages_turn()];
    let input = crate::api::encode_responses(&turns, "", crate::api::PromptCachePolicy::None);
    assert_eq!(input.len(), 1, "only the user turn survives: {input:?}");
}
