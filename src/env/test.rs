use super::*;

#[test]
fn system_prompt_appends_role_section_when_present() {
    let without = system_prompt("m", "sample-session", &Lineage::default(), None, None, None);
    let with = system_prompt(
        "m",
        "sample-session",
        &Lineage::default(),
        None,
        Some("Your role for this session:\n\nStay put."),
        None,
    );
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
        "m",
        "sample-session",
        &Lineage::default(),
        Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
        Some("Your role for this session:\n\nDo it."),
        None,
    );
    assert!(s.contains("<CLAUDE.md>\nhi\n</CLAUDE.md>"));
    assert!(s.find("<CLAUDE.md>").unwrap() < s.find("Your role").unwrap());
    // No block → nothing spliced.
    assert!(
        !system_prompt("m", "sample-session", &Lineage::default(), None, None, None)
            .contains("<CLAUDE.md>")
    );
}

/// The session's config file is never named in the system prompt: the permission perimeter
/// stays out of the agent's attention until a move strays and the gate surfaces it. Guards
/// against reintroducing the up-front config mention a dogfood session found distracting.
#[test]
fn system_prompt_never_names_the_config_file() {
    let full = system_prompt(
        "m",
        "sample-session",
        &Lineage::default(),
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
        "m",
        "sample-session",
        &Lineage::default(),
        Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
        Some("Your role for this session:\n\nDo it."),
        Some(unmounted),
    );
    assert!(full.ends_with(unmounted));
    assert!(full.find("Your role").unwrap() < full.find("Declared but unavailable").unwrap());

    // Nothing unmounted → byte-identical to the prompt without the parameter.
    let without = system_prompt(
        "m",
        "sample-session",
        &Lineage::default(),
        Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
        Some("Your role for this session:\n\nDo it."),
        None,
    );
    assert!(!without.contains("Declared but unavailable"));
}

/// The identity line is unconditional and names the configured model verbatim,
/// ahead of the context block — an untold model answers "which model are you"
/// from its priors (docs/context.md).
///
/// **The opening line's exact bytes.** It leads the cached prefix, so a reworded
/// sentence moves every session's prefix; this pins it so the rewording breaks a
/// test first (docs/log-and-projection.md). The grammar is load-bearing: one
/// identity claim (`You are {model}.`) and one possession (`This session is
/// {id}`), never two identity claims, which a model resolves by treating the
/// generated id as a persona to inhabit.
#[test]
fn system_prompt_states_the_model_identity_and_the_session_id() {
    let s = system_prompt(
        "anthropic/claude-sonnet-5",
        "burgundy-skink",
        &Lineage::default(),
        Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
        None,
        None,
    );
    assert!(s.starts_with(concat!(
        "You are anthropic/claude-sonnet-5. ",
        "This session is `burgundy-skink` on this host; ",
        "that is the id other agents use to reach you.\n\n",
    )));
    assert!(s.find("You are ").unwrap() < s.find("<CLAUDE.md>").unwrap());
    // One identity claim, not two: the session *has* an id.
    assert_eq!(s.matches("You are ").count(), 1);
    assert!(!s.contains("Your name is"));
    // A root session has no creator, so the creator sentence is absent
    // entirely — not rendered with an empty or placeholder id.
    assert!(!s.contains("created by"));
}

/// The creator sentence's exact bytes, on the same line and under the same rules
/// as the id it follows (docs/context.md). It states the relationship and stops:
/// the creator id is a return address, not an authority grant and not a claim
/// the creator is still running. Present only when a session commissioned this
/// one; a root renders the identity line alone, byte-for-byte.
#[test]
fn system_prompt_names_the_creating_session_when_there_is_one() {
    let child = system_prompt(
        "anthropic/claude-sonnet-5",
        "eager-meadow",
        &Lineage {
            created_by: Some(String::from("lucid-lark")),
            ..Lineage::default()
        },
        Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
        None,
        None,
    );
    assert!(child.starts_with(concat!(
        "You are anthropic/claude-sonnet-5. ",
        "This session is `eager-meadow` on this host; ",
        "that is the id other agents use to reach you. ",
        "It was created by `lucid-lark`, which you can reach the same way.\n\n",
    )));
    // Still one identity claim across both sentences: the creator is named as a
    // relation, never as a second "you are".
    assert_eq!(child.matches("You are ").count(), 1);
    // The creator is named once. A second mention is where changing peer status
    // ("is waiting on you", "has three processes running") would accrete, and
    // the prefix is frozen for the session's life.
    assert_eq!(child.matches("lucid-lark").count(), 1);

    // A root: the sentence is absent, and the rest is byte-identical to the
    // child's prompt with that one sentence removed.
    let root = system_prompt(
        "anthropic/claude-sonnet-5",
        "eager-meadow",
        &Lineage::default(),
        Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
        None,
        None,
    );
    assert!(!root.contains("created by"));
    assert_eq!(
        child.replace(
            " It was created by `lucid-lark`, which you can reach the same way.",
            ""
        ),
        root
    );

    // Detaching is host control state, not provenance: the prompt takes only the
    // recorded creator id, so a detached child renders the same sentence as an
    // ordinary one. Nothing downstream of this id says which it is.
    assert_eq!(
        child,
        system_prompt(
            "anthropic/claude-sonnet-5",
            "eager-meadow",
            &Lineage {
                created_by: Some(String::from("lucid-lark")),
                ..Lineage::default()
            },
            Some("<CLAUDE.md>\nhi\n</CLAUDE.md>"),
            None,
            None,
        )
    );
}

/// The separate-model peek's system prompt (docs/redaction-fork.md): the triage
/// model's own identity line, the agent named where the seam is disclosed, the
/// context block verbatim, and none of the agent's curation preamble.
#[test]
fn triage_system_prompt_names_both_models_and_carries_the_context() {
    let prompt = triage_system_prompt("small", "big", Some("<ctx>x</ctx>"));
    assert!(prompt.starts_with("You are small.\n\n"));
    assert!(prompt.contains("produced by the agent, big,"));
    assert!(prompt.ends_with("\n\n<ctx>x</ctx>"));
    assert!(!prompt.contains(PREAMBLE.trim_end()));
    assert!(!triage_system_prompt("small", "big", None).ends_with("\n\n"));
}

/// A successor's identity line names its predecessor, says the first message is
/// that session's brief, and says archiving it is the successor's to do
/// (docs/tools.md).
#[test]
fn a_successor_is_told_whom_it_succeeds() {
    let prompt = system_prompt(
        "m",
        "new-leaf",
        &Lineage {
            created_by: None,
            succeeds: Some("old-leaf".into()),
        },
        None,
        None,
        None,
    );
    assert!(prompt.contains("It succeeds `old-leaf`"), "{prompt}");
    assert!(prompt.contains("the first message is that session's brief"));
    assert!(prompt.contains("not the operator's instruction"));
    assert!(prompt.contains("confirm what they want to do"));
    assert!(prompt.contains("archive it with `archive`"));
    assert!(!prompt.contains("It was created by"));
}
