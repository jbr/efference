//! The submitted turn's document form (docs/control-api.md): what a turn
//! lands as, what an `ask` reply keeps beside its readable text, and the
//! one-definition-per-upload rule a restaged recall relies on.

use super::*;
use crate::record::log::attachment::{self, Arrival};
use std::collections::BTreeMap;

fn arrival(name: &str) -> Arrival {
    Arrival {
        name: name.into(),
        path: format!("att/composer/{name}"),
        kind: attachment::Kind::Binary { bytes: 3 },
    }
}

fn agent_document() -> UserContent {
    UserContent {
        content: vec![
            UserSegment::Text("look ".into()),
            UserSegment::Reference("a".into()),
        ],
        references: BTreeMap::from([(
            "a".into(),
            ReferenceDefinition::agent("reviewer", "castback"),
        )]),
    }
}

#[test]
fn a_structured_turn_lands_as_one_document() {
    let document = agent_document();
    let turn = SubmittedTurn::structured(document.clone(), "s1");
    assert_eq!(turn.document(), Some(document.clone()));
    assert_eq!(turn.readable(), "look @castback");
    assert!(matches!(
        turn.payload(),
        Payload::User(blocks)
            if blocks == vec![ContentBlock::Document(document)]
    ));
}

#[test]
fn a_plain_turn_keeps_no_document() {
    let turn = SubmittedTurn::new("hello", "s1");
    assert!(turn.document().is_none(), "plain stays byte-for-byte");
    assert_eq!(turn.reply_text(), "hello");
}

#[test]
fn with_attachments_defines_each_upload_once() {
    let first =
        SubmittedTurn::new("look", "s1").with_attachments(vec![arrival("a.txt"), arrival("b.txt")]);
    assert_eq!(first.attachment_arrivals().len(), 2);
    // A recalled documented turn's chips are staged again on the way back;
    // resubmitting them must not define the same store upload twice.
    let again = first.clone().with_attachments(vec![arrival("a.txt")]);
    assert_eq!(again.attachment_arrivals().len(), 2);
    assert_eq!(again.readable(), first.readable());
}

#[test]
fn an_attachment_only_reply_text_names_the_upload() {
    let turn = SubmittedTurn::new("", "s1").with_attachments(vec![arrival("shot.png")]);
    assert!(turn.document().is_some(), "an occurrence is not plain");
    let text = turn.reply_text();
    assert!(
        text.contains("shot.png"),
        "the notice line carries it: {text}"
    );
    assert_eq!(turn.readable(), "");
}
