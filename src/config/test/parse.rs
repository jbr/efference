//! Tests for `parse.rs`.

use super::*;

#[test]
fn empty_document_is_a_valid_default_config() {
    let role = parse_str("");
    assert!(role.scope.is_none());
    assert!(role.watch_for.is_empty());
    assert!(role.trust_threshold.is_none());
    assert!(role.commands.is_empty());
    assert!(role.tools.is_empty());
    assert!(role.paths.is_empty());
    assert!(role.extends.is_empty());
    assert!(role.context.is_empty());
}

#[test]
fn parses_scope_watch_for_and_trust_threshold() {
    let role = parse_str(
        r#"
            scope = "Work only inside this repo."
            watch-for = ["Flag network egress to non-registry hosts."]
            trust-threshold = 0.5
            "#,
    );
    assert_eq!(role.scope.as_deref(), Some("Work only inside this repo."));
    assert_eq!(
        role.watch_for,
        ["Flag network egress to non-registry hosts."]
    );
    assert_eq!(role.trust_threshold, Some(0.5));
}

#[test]
fn parses_blanket_and_sub_verb_command_keys() {
    let role = parse_str(
        r#"
            [commands]
            cargo = true
            "git diff" = true
            "git status" = true
            "#,
    );
    assert!(role.commands.contains("cargo"));
    assert!(role.commands.contains("git diff"));
    assert!(role.commands.contains("git status"));
    assert_eq!(role.commands.len(), 3);
}

#[test]
fn parses_tool_grants() {
    let role = parse_str(r#"tools = ["read", "write", "kill_process"]"#);
    assert!(role.tools.contains("read"));
    assert!(role.tools.contains("write"));
    assert!(role.tools.contains("kill_process"));
}

#[test]
fn parses_path_grants_with_sugar_and_explicit_perms() {
    let role = parse_str(
        r#"
            [paths]
            "." = true
            "/tmp" = { read = true }
            "vendor" = false
            "#,
    );
    assert_eq!(role.paths.len(), 3);
    // cwd sugar → full access.
    assert_eq!(
        role.paths["."],
        Perms {
            read: true,
            write: true
        }
    );
    // /tmp: read-only (write defaults false).
    assert_eq!(
        role.paths["/tmp"],
        Perms {
            read: true,
            write: false
        }
    );
    // `false` grants nothing until you say what you're granting.
    assert_eq!(
        role.paths["vendor"],
        Perms {
            read: false,
            write: false
        }
    );
}

#[test]
fn parses_reserved_extends_pointer() {
    let role = parse_str(r#"extends = ["base.toml"]"#);
    assert_eq!(role.extends, ["base.toml"]);
}

/// The three legal entry shapes decode to the right `ContextEntry`: a bare read-only path, a
/// bare updatable path (key-is-path table), and a labeled group with `path` + write grants.
#[test]
fn parses_context_entry_shapes() {
    let role = parse_str(
        r#"
            [context]
            "./CLAUDE.md" = true
            "./ARCHITECTURE.md" = { update = true }
            Memories = { path = "./memories/", create = true, update = true }
            "#,
    );
    assert_eq!(
        role.context,
        vec![
            ContextEntry {
                heading: None,
                source: ContextSource::Files(vec!["./CLAUDE.md".into()]),
                grants: ContextGrants::default(),
                sort: None,
            },
            ContextEntry {
                heading: None,
                source: ContextSource::Files(vec!["./ARCHITECTURE.md".into()]),
                grants: ContextGrants {
                    update: true,
                    create: false,
                },
                sort: None,
            },
            ContextEntry {
                heading: Some("Memories".into()),
                source: ContextSource::Files(vec!["./memories/".into()]),
                grants: ContextGrants {
                    update: true,
                    create: true,
                },
                sort: None,
            },
        ],
    );
}

/// A labeled group's `paths` list and a `sort` override decode; `path` then `paths` join in order.
#[test]
fn parses_context_group_with_paths_and_sort() {
    let role = parse_str(
        r#"
            [context]
            Skills = { paths = ["a/*.md", "b/*.md"], sort = "modified desc" }
            "#,
    );
    assert_eq!(
        role.context,
        vec![ContextEntry {
            heading: Some("Skills".into()),
            source: ContextSource::Files(vec!["a/*.md".into(), "b/*.md".into()]),
            grants: ContextGrants::default(),
            sort: Some("modified desc".into()),
        }],
    );
}

/// An `exec` entry is a labeled dynamic section with no files, grants, or sort.
#[test]
fn parses_context_exec_entry() {
    let role = parse_str(
        r#"
            [context]
            Date = { exec = "date" }
            "#,
    );
    assert_eq!(
        role.context,
        vec![ContextEntry {
            heading: Some("Date".into()),
            source: ContextSource::Exec("date".into()),
            grants: ContextGrants::default(),
            sort: None,
        }],
    );
}

/// **Document order is preserved** — the `[context]` entries come out in file order, not sorted
/// (they render as preamble sections positionally; an `exec` can sit between static files). The
/// keys here are deliberately anti-alphabetical to catch a sorted-map regression.
#[test]
fn context_preserves_document_order() {
    let role = parse_str(
        r#"
            [context]
            "zzz.md" = true
            Middle = { exec = "date" }
            "aaa.md" = true
            "#,
    );
    let headings_and_sources: Vec<_> = role
        .context
        .iter()
        .map(|e| (e.heading.clone(), e.source.clone()))
        .collect();
    assert_eq!(
        headings_and_sources,
        vec![
            (None, ContextSource::Files(vec!["zzz.md".into()])),
            (Some("Middle".into()), ContextSource::Exec("date".into())),
            (None, ContextSource::Files(vec!["aaa.md".into()])),
        ],
    );
}

/// `create` on a bare path (key-is-path, no `path`/`paths`) fails — it's group-only.
#[test]
fn context_create_on_bare_path_fails() {
    let err = super::parse(
        Path::new("bad.toml"),
        "[context]\n\"./notes.md\" = { create = true }\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::ContextBarePathGrant(_, "create"))
    ));
}

/// `sort` on a bare path fails for the same reason — nothing to order.
#[test]
fn context_sort_on_bare_path_fails() {
    let err = super::parse(
        Path::new("bad.toml"),
        "[context]\n\"./notes.md\" = { sort = \"modified desc\" }\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::ContextBarePathGrant(_, "sort"))
    ));
}

/// `exec` mixed with a file source is ambiguous and fails.
#[test]
fn context_exec_with_files_fails() {
    let err = super::parse(
        Path::new("bad.toml"),
        "[context]\nBad = { exec = \"date\", path = \"./x\" }\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::ContextExecConflict(_))
    ));
}

/// A `false` context entry is rejected — nothing to disable.
#[test]
fn context_false_is_rejected() {
    let err = super::parse(Path::new("bad.toml"), "[context]\n\"./x.md\" = false\n").unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::ContextDisabled(_))
    ));
}

#[test]
fn malformed_toml_is_a_parse_error_not_a_panic() {
    let err = super::parse(Path::new("bad.toml"), "scope = ").unwrap_err();
    assert!(matches!(err, LoadError::Parse(..)));
}

#[test]
fn unknown_key_is_rejected() {
    // A typo'd key should fail loudly rather than be silently ignored — the typed schema
    // is the contract (deny_unknown_fields).
    let err = super::parse(Path::new("bad.toml"), r#"breif = "typo""#).unwrap_err();
    assert!(matches!(err, LoadError::Parse(..)));
}

/// The soundness gate: a `[commands]` key that isn't a single simple command fails the
/// whole file — no silent skip, no partial recovery.
#[test]
fn non_simple_command_key_fails_the_file() {
    let err = super::parse(Path::new("bad.toml"), "[commands]\n\"git | rm\" = true\n").unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::CommandNotSimple(_))
    ));
}

/// A `false` command value is rejected (additive-only can't deny).
#[test]
fn command_deny_is_rejected() {
    let err = super::parse(Path::new("bad.toml"), "[commands]\ncargo = false\n").unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::CommandDeny(_))
    ));
}
