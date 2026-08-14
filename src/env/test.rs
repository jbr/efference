use super::*;

#[test]
fn system_prompt_appends_role_section_when_present() {
    let without = system_prompt(None, None, None);
    let with = system_prompt(None, Some("Your role for this session:\n\nStay put."), None);
    // The role section is appended verbatim after the env block; without a role the
    // prompt is exactly the preamble + env (no trailing role text).
    assert!(with.starts_with(&without));
    assert!(with.ends_with("Your role for this session:\n\nStay put."));
    assert!(!without.contains("Your role for this session"));
}

/// The `[context]` block splices in after the curation preamble and **before** the role/scope —
/// knowledge before perimeter (docs/context.md).
#[test]
fn system_prompt_splices_context_block_before_the_role() {
    let s = system_prompt(
        Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
        Some("Your role for this session:\n\nDo it."),
        None,
    );
    assert!(s.contains("<CLAUDE.md>\nhi\n</CLAUDE.md>"));
    assert!(s.find("<CLAUDE.md>").unwrap() < s.find("Your role").unwrap());
    // No block → nothing spliced.
    assert!(!system_prompt(None, None, None).contains("<CLAUDE.md>"));
}

/// The session's config file is never named in the system prompt: the permission perimeter
/// stays out of the agent's attention until a move strays and the gate surfaces it. Guards
/// against reintroducing the up-front config mention a dogfood session found distracting.
#[test]
fn system_prompt_never_names_the_config_file() {
    let full = system_prompt(
        Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
        Some("Your role for this session:\n\nDo it."),
        None,
    );
    assert!(!full.contains(".toml"));
    assert!(!full.to_lowercase().contains("permission"));
    assert!(!full.contains("request a change"));
}

/// What the session does *not* have goes last, next to the perimeter it
/// qualifies — and only when there is something to say, so the common case
/// leaves the cached prefix untouched.
#[test]
fn system_prompt_appends_the_unmounted_section_last() {
    let unmounted = "# Declared but unavailable\n\n- `sentry` — needs authorization";
    let full = system_prompt(
        Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
        Some("Your role for this session:\n\nDo it."),
        Some(unmounted),
    );
    assert!(full.ends_with(unmounted));
    assert!(full.find("Your role").unwrap() < full.find("Declared but unavailable").unwrap());

    // Nothing unmounted → byte-identical to the prompt without the parameter.
    let without = system_prompt(
        Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
        Some("Your role for this session:\n\nDo it."),
        None,
    );
    assert!(!without.contains("Declared but unavailable"));
}
