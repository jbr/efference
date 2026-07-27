//! Tests for `frontmatter.rs`.

use super::*;

#[test]
fn no_frontmatter_is_the_whole_body_and_inlines() {
    let text = "# Title\n\nBody paragraph.\n";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.frontmatter, None);
    assert_eq!(split.body, text);
    assert_eq!(split.mode("file"), Mode::Inline);
}

#[test]
fn frontmatter_with_description_lists_and_splits_the_body() {
    let text = "---\nname: verify\ndescription: Drive the flow end to end.\n---\nThe body.\n";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.frontmatter,
        Some(Frontmatter {
            name: Some("verify".into()),
            description: Some("Drive the flow end to end.".into()),
            inline: None,
        })
    );
    assert_eq!(split.body, "The body.\n");
    assert_eq!(
        split.mode("fallback"),
        Mode::Listed {
            name: "verify".into(),
            description: "Drive the flow end to end.".into(),
        }
    );
}

#[test]
fn description_without_name_uses_the_fallback() {
    let text = "---\ndescription: A thing.\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.mode("my-doc"),
        Mode::Listed {
            name: "my-doc".into(),
            description: "A thing.".into(),
        }
    );
}

#[test]
fn inline_true_overrides_a_description_back_to_inline() {
    let text = "---\ndescription: A thing.\ninline: true\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.frontmatter.as_ref().unwrap().inline, Some(true));
    assert_eq!(split.mode("d"), Mode::Inline);
}

#[test]
fn frontmatter_without_a_description_inlines() {
    // A name but no description: nothing to catalog, so it inlines.
    let text = "---\nname: just-a-name\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.mode("d"), Mode::Inline);
}

#[test]
fn a_dash_rule_mid_document_is_not_frontmatter() {
    // The file does not *start* with the delimiter, so the `---` below is a horizontal rule.
    let text = "# Title\n\n---\n\nMore text.\n";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.frontmatter, None);
    assert_eq!(split.body, text);
}

#[test]
fn unterminated_frontmatter_is_an_error() {
    let text = "---\nname: oops\nno closing delimiter\n";
    assert_eq!(split_frontmatter(text), Err(UnterminatedFrontmatter));
}

#[test]
fn a_value_may_contain_colons_and_be_quoted() {
    let text = "---\ndescription: \"Use this: for X, Y, and Z\"\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.frontmatter.unwrap().description.as_deref(),
        Some("Use this: for X, Y, and Z"),
    );
}

/// The shape real Claude-Code skill files use: `description: >-` folded over several
/// indented lines. The folded value joins with spaces and still drives listed mode.
#[test]
fn folded_block_scalar_description_lists() {
    let text = "---\nname: ferritin\ndescription: >-\n  Look up real Rust documentation\n  \
                    instead of guessing APIs.\n---\nbody\n";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.mode("fallback"),
        Mode::Listed {
            name: "ferritin".into(),
            description: "Look up real Rust documentation instead of guessing APIs.".into(),
        }
    );
    assert_eq!(split.body, "body\n");
}

/// A literal (`|`) block scalar keeps its line breaks; the chomp indicator is ignored.
#[test]
fn literal_block_scalar_keeps_newlines() {
    let text = "---\ndescription: |-\n  line one\n  line two\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.frontmatter.unwrap().description.as_deref(),
        Some("line one\nline two")
    );
}

/// An unknown key's block value is consumed whole — an indented `description:` line inside
/// it is that block's content, never a frontmatter key of its own.
#[test]
fn unknown_key_block_scalar_cannot_pollute_known_keys() {
    let text = "---\nfuture_key: >-\n  description: sneaky\n  more text\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.mode("d"), Mode::Inline);
    assert_eq!(split.frontmatter.unwrap().description, None);
}

/// A block scalar closed by the frontmatter terminator itself (no flush-left line after it)
/// still lands.
#[test]
fn block_scalar_closed_by_the_terminator() {
    let text = "---\ndescription: >\n  right at the end\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.frontmatter.unwrap().description.as_deref(),
        Some("right at the end")
    );
}

/// A nested map's interior lines are indented, so its `name:`/`description:` never pollute the
/// top-level keys — the shape of a memory file's `metadata:` map.
#[test]
fn nested_map_keys_cannot_pollute_top_level_keys() {
    let text = "---\nmetadata:\n  name: sneaky\n  type: user\ndescription: Real one.\n---\nb";
    let split = split_frontmatter(text).unwrap();
    let fm = split.frontmatter.unwrap();
    assert_eq!(fm.name, None);
    assert_eq!(fm.description.as_deref(), Some("Real one."));
}

#[test]
fn unknown_frontmatter_keys_are_ignored() {
    let text = "---\ndescription: A.\nfuture_key: whatever\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    let fm = split.frontmatter.unwrap();
    assert_eq!(fm.description.as_deref(), Some("A."));
    assert_eq!(fm.name, None);
    assert_eq!(fm.inline, None);
}

#[test]
fn empty_frontmatter_block_inlines() {
    let text = "---\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.frontmatter, Some(Frontmatter::default()));
    assert_eq!(split.body, "body");
    assert_eq!(split.mode("d"), Mode::Inline);
}

#[test]
fn fallback_name_is_the_stem() {
    assert_eq!(fallback_name(Path::new("docs/verify.md")), "verify");
    assert_eq!(fallback_name(Path::new("CLAUDE.md")), "CLAUDE");
}
