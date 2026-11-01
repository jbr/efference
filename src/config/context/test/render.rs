//! Tests for `render.rs`.

use super::*;

#[test]
fn empty_resolved_renders_none() {
    assert_eq!(Resolved::default().render(), None);
}

/// The full render pins the tagged bytes: a bare inlined file is its own `<name>` tag; a labeled
/// group wraps its listed member(s); sections join with a blank line, in document order. Pins the
/// structure by referencing `LEAD_IN`, so the lead-in wording can change without breaking it.
#[test]
fn render_pins_the_tagged_bytes() {
    let resolved = Resolved {
        sections: vec![
            Section {
                heading: None,
                description: None,
                items: vec![Item::Inline {
                    name: "CLAUDE.md".into(),
                    path: "CLAUDE.md".into(),
                    body: "# efference\n\nThe harness.".into(),
                }],
            },
            Section {
                heading: Some("Skills".into()),
                description: None,
                items: vec![Item::Listed {
                    name: "verify".into(),
                    description: "Drive the flow end to end.".into(),
                    path: "skills/verify.md".into(),
                }],
            },
        ],
    };
    let expected = format!(
        "{LEAD_IN}\n\n\
             <CLAUDE.md>\n# efference\n\nThe harness.\n</CLAUDE.md>\n\n\
             <Skills>\n<verify path=\"skills/verify.md\">Drive the flow end to end.</verify>\n</Skills>"
    );
    assert_eq!(resolved.render().as_deref(), Some(expected.as_str()));
    let resolved = Resolved {
        sections: vec![
            Section {
                heading: None,
                description: None,
                items: vec![Item::Inline {
                    name: "CLAUDE.md".into(),
                    path: "./CLAUDE.md".into(),
                    body: "# efference\n\nThe harness.\n".into(),
                }],
            },
            Section {
                heading: Some("Skills".into()),
                description: None,
                items: vec![Item::Listed {
                    name: "verify".into(),
                    description: "Drive the flow end to end.".into(),
                    path: "skills/verify.md".into(),
                }],
            },
        ],
    };
    let expected = format!(
        "{LEAD_IN}\n\n\
             <CLAUDE.md>\n# efference\n\nThe harness.\n</CLAUDE.md>\n\n\
             <Skills>\n<verify path=\"skills/verify.md\">Drive the flow end to end.</verify>\n</Skills>"
    );
    assert_eq!(resolved.render().as_deref(), Some(expected.as_str()));
}

/// The operator's heading is the tag verbatim — spaces and all, since it's pseudo-XML nothing
/// parses.
#[test]
fn render_uses_the_heading_verbatim_as_a_tag() {
    let resolved = Resolved {
        sections: vec![Section {
            heading: Some("Project Tips".into()),
            description: None,
            items: vec![Item::Listed {
                name: "style".into(),
                description: "Two-space indent.".into(),
                path: "tips/style.md".into(),
            }],
        }],
    };
    assert!(resolved.render().unwrap().contains(
        "<Project Tips>\n<style path=\"tips/style.md\">Two-space indent.</style>\n</Project Tips>"
    ));
}

/// A section's `description` renders as a `<description>` child element inside the group wrapper,
/// before the file items — so the model sees the prose hint before the file listing.
#[test]
fn render_group_description_as_child_element() {
    let resolved = Resolved {
        sections: vec![Section {
            heading: Some("Memories".into()),
            description: Some("Use the read and write tools to access these files.".into()),
            items: vec![Item::Listed {
                name: "design-decisions".into(),
                description: "How we work.".into(),
                path: "memory/design-decisions.md".into(),
            }],
        }],
    };
    let rendered = resolved.render().unwrap();
    assert!(rendered.contains(
        "<Memories>\n<description>Use the read and write tools to access these files.</description>\n<design-decisions"
    ));
    // The description must come before the first item.
    assert!(rendered.find("<description>").unwrap() < rendered.find("<design-decisions").unwrap());
}
