//! Tests for `preamble.rs`.

use super::*;

#[test]
fn preamble_section_renders_the_scope_byte_for_byte() {
    let role = parse_str(r#"scope = "Stay in the repo.""#);
    assert_eq!(
        role.preamble_section().as_deref(),
        Some("Your role for this session:\n\nStay in the repo.")
    );
}

#[test]
fn preamble_section_is_none_without_a_scope() {
    assert!(Config::default().preamble_section().is_none());
    // A whitespace-only scope is treated as no scope (no empty section on the wire).
    let role = parse_str(r#"scope = "   ""#);
    assert!(role.preamble_section().is_none());
}

/// The watch-for list is spotter-only — it must never leak into the session's preamble.
#[test]
fn preamble_section_excludes_the_watch_for() {
    let role = parse_str(
        r#"
            scope = "Do the task."
            watch-for = ["SECRET RED FLAGS"]
            "#,
    );
    let section = role.preamble_section().unwrap();
    assert!(section.contains("Do the task."));
    assert!(!section.contains("SECRET RED FLAGS"));
}
