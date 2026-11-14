//! Tests for the shared reference contract's submission shape
//! (docs/log-and-projection.md): access normalization and defaults, the
//! filesystem submission round-tripping what the client supplied, and the
//! attachment and agent shapes the gate resolves or validates.

use super::*;

#[test]
fn access_normalizes_write_to_read() {
    assert_eq!(
        ReferenceAccess {
            read: true,
            write: true
        }
        .normalized(),
        Some(ReferenceAccess {
            read: true,
            write: true
        })
    );
    // Write alone implies read.
    assert_eq!(
        ReferenceAccess {
            read: false,
            write: true
        }
        .normalized(),
        Some(ReferenceAccess {
            read: true,
            write: true
        })
    );
    assert_eq!(
        ReferenceAccess {
            read: true,
            write: false
        }
        .normalized(),
        Some(ReferenceAccess {
            read: true,
            write: false
        })
    );
    // No read is not a reference; the gate refuses it.
    assert_eq!(
        ReferenceAccess {
            read: false,
            write: false
        }
        .normalized(),
        None
    );
}

#[test]
fn access_requests_render_normalized() {
    // The rendered request states the contract's form, not the raw booleans:
    // write alone renders as read+write.
    assert_eq!(
        ReferenceAccess {
            read: false,
            write: true
        }
        .requested(),
        "read+write"
    );
    assert_eq!(ReferenceAccess::default().requested(), "read");
    assert_eq!(
        ReferenceAccess {
            read: false,
            write: false
        }
        .requested(),
        "none"
    );
}

#[test]
fn omitted_access_defaults_to_read() {
    let target: SubmittedTarget = serde_json::from_str(
        r#"{"type":"filesystem","original":"notes.md","path":"/tmp/notes.md","target":"file"}"#,
    )
    .unwrap();
    assert_eq!(
        target,
        SubmittedTarget::Filesystem {
            original: "notes.md".into(),
            path: "/tmp/notes.md".into(),
            target: ReferenceTargetKind::File,
            access: ReferenceAccess::default(),
        }
    );
}

#[test]
fn submitted_filesystem_round_trips_resolved_path() {
    let stored = ReferenceDefinition {
        label: "notes".into(),
        target: ReferenceTarget::Filesystem {
            original: "notes.md".into(),
            path: "/tmp/notes.md".into(),
            target: ReferenceTargetKind::File,
            access: ReferenceAccess {
                read: true,
                write: true,
            },
        },
    };
    // A recalled target is resubmitted with the client-supplied absolute path
    // and access unchanged: the gate validates them and never re-derives the
    // path from `original`.
    assert_eq!(
        stored.submitted().target,
        SubmittedTarget::Filesystem {
            original: "notes.md".into(),
            path: "/tmp/notes.md".into(),
            target: ReferenceTargetKind::File,
            access: ReferenceAccess {
                read: true,
                write: true,
            },
        }
    );
}
#[test]
fn attachment_submission_keeps_only_the_store_name() {
    let arrival = Arrival {
        name: "shot.png".into(),
        path: "/store/shot.png".into(),
        kind: attachment::Kind::Binary { bytes: 3 },
    };
    let stored = ReferenceDefinition::attachment(arrival);
    assert_eq!(stored.label, "shot.png");
    assert_eq!(
        stored.submitted(),
        SubmittedReference {
            label: "shot.png".into(),
            target: SubmittedTarget::Attachment {
                name: "shot.png".into(),
            },
        }
    );
    // An attachment is a stored upload, not prose: the readable flat form
    // spells nothing, and callers show its notice or name separately.
    assert_eq!(stored.readable(), "");
    assert_eq!(
        stored.describe_line(),
        "shot.png — binary, 3 B — at /store/shot.png"
    );
}

#[test]
fn agent_submission_keeps_the_session_id() {
    let stored = ReferenceDefinition::agent("the reviewer", "castback");
    assert_eq!(stored.readable(), "@castback");
    assert_eq!(
        stored.submitted(),
        SubmittedReference {
            label: "the reviewer".into(),
            target: SubmittedTarget::Agent {
                session: "castback".into(),
            },
        }
    );
}

#[test]
fn readable_keeps_prose_and_render_lists_every_definition() {
    use crate::record::log::{UserContent, UserSegment};
    use std::collections::BTreeMap;
    let arrival = Arrival {
        name: "shot.png".into(),
        path: "/store/shot.png".into(),
        kind: attachment::Kind::Binary { bytes: 3 },
    };
    let document = UserContent {
        content: vec![
            UserSegment::Text("see ".into()),
            UserSegment::Reference("a".into()),
            UserSegment::Text(" and ".into()),
            UserSegment::Reference("b".into()),
        ],
        references: BTreeMap::from([
            (
                "a".into(),
                ReferenceDefinition::agent("reviewer", "castback"),
            ),
            ("b".into(), ReferenceDefinition::attachment(arrival.clone())),
        ]),
    };
    // The flat form concatenates prose and chips with no separator invented
    // between them; the attachment contributes no prose at all.
    assert_eq!(document.readable(), "see @castback and ");
    assert_eq!(document.attachment_arrivals(), vec![arrival]);
    // One string, with every definition's metadata after the prose: this is
    // where an attachment's identity reaches a fallback that uses `render`.
    assert_eq!(
        document.render(None),
        "see [a] and [b]\n\nreferences:\n[a] reviewer — @castback\n\
         [b] shot.png — binary, 3 B — at /store/shot.png"
    );
}

/// A document with an agent occurrence between two prose pieces, plus an
/// attachment occurrence at the end — the shape a held recall's edit is
/// applied to.
fn editable_document() -> crate::record::log::UserContent {
    use crate::record::log::{UserContent, UserSegment};
    use std::collections::BTreeMap;
    let arrival = Arrival {
        name: "shot.png".into(),
        path: "/store/shot.png".into(),
        kind: attachment::Kind::Binary { bytes: 3 },
    };
    UserContent {
        content: vec![
            UserSegment::Text("look at ".into()),
            UserSegment::Reference("a".into()),
            UserSegment::Text(" please".into()),
            UserSegment::Reference("b".into()),
        ],
        references: BTreeMap::from([
            (
                "a".into(),
                ReferenceDefinition::agent("reviewer", "castback"),
            ),
            ("b".into(), ReferenceDefinition::attachment(arrival)),
        ]),
    }
}

#[test]
fn an_edit_after_an_occurrence_keeps_it() {
    let document = editable_document();
    let old = document.readable();
    assert_eq!(old, "look at @castback please");
    let edited = document.apply_text_edit(&old, "look at @castback now");
    assert_eq!(edited.readable(), "look at @castback now");
    assert!(edited.references.contains_key("a"));
    assert!(edited.references.contains_key("b"));
}

#[test]
fn an_edit_reaching_an_occurrence_replaces_it_with_text() {
    let document = editable_document();
    let old = document.readable();
    // Deleting from the chip onward reaches the agent occurrence; the
    // attachment's zero-width occurrence sits at the deleted range's end and
    // survives, so the upload is still referenced.
    let edited = document.apply_text_edit(&old, "look");
    assert_eq!(edited.readable(), "look");
    assert!(!edited.references.contains_key("a"));
    assert!(edited.references.contains_key("b"));
}

#[test]
fn an_edit_inside_a_occurrence_drops_it() {
    let document = editable_document();
    let old = document.readable();
    let edited = document.apply_text_edit(&old, "look at @other please");
    assert_eq!(edited.readable(), "look at @other please");
    assert!(!edited.references.contains_key("a"));
    // The attachment occurrence sits after the edit, so it stays.
    assert!(edited.references.contains_key("b"));
}

#[test]
fn typing_into_an_attachment_only_document_keeps_the_upload() {
    use crate::record::log::{UserContent, UserSegment};
    use std::collections::BTreeMap;
    let arrival = Arrival {
        name: "shot.png".into(),
        path: "/store/shot.png".into(),
        kind: attachment::Kind::Binary { bytes: 3 },
    };
    let document = UserContent {
        content: vec![UserSegment::Reference("b".into())],
        references: BTreeMap::from([("b".into(), ReferenceDefinition::attachment(arrival))]),
    };
    let edited = document.apply_text_edit("", "describe this");
    assert_eq!(edited.readable(), "describe this");
    assert_eq!(edited.attachment_arrivals().len(), 1);
    assert!(edited.content.contains(&UserSegment::Reference("b".into())));
}

#[test]
fn a_multibyte_edit_stays_whole() {
    let document = editable_document();
    let old = document.readable();
    // Replace the multibyte tail; the prefix ends on a char boundary.
    let edited = document.apply_text_edit(&old, "look at @castback pléase");
    assert_eq!(edited.readable(), "look at @castback pléase");
    assert!(edited.references.contains_key("a"));
}

#[test]
fn a_cleared_draft_drops_the_prose_occurrences() {
    let document = editable_document();
    let edited = document.apply_text_edit(&document.readable(), "");
    assert_eq!(edited.readable(), "");
    assert!(!edited.references.contains_key("a"));
    // Clearing prose does not drop an upload: the attachment's occurrence is
    // zero-width and the hold's explicit clear is the client's way out.
    assert!(edited.references.contains_key("b"));
}
