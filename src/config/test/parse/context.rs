//! Tests for the `[context]` table.

use super::*;

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
                exclude: Vec::new(),
                description: None,
            },
            ContextEntry {
                heading: None,
                source: ContextSource::Files(vec!["./ARCHITECTURE.md".into()]),
                grants: ContextGrants {
                    update: true,
                    create: false,
                },
                sort: None,
                exclude: Vec::new(),
                description: None,
            },
            ContextEntry {
                heading: Some("Memories".into()),
                source: ContextSource::Files(vec!["./memories/".into()]),
                grants: ContextGrants {
                    update: true,
                    create: true,
                },
                sort: None,
                exclude: Vec::new(),
                description: None,
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
            exclude: Vec::new(),
            description: None,
        }],
    );
}

/// A labeled group's `description` — a prose hint for the model about how to use the files.
#[test]
fn parses_context_group_with_description() {
    let role = parse_str(
        r#"
            [context]
            Memories = { path = "./memories/", description = "Use the read and write tools to access." }
            "#,
    );
    assert_eq!(
        role.context,
        vec![ContextEntry {
            heading: Some("Memories".into()),
            source: ContextSource::Files(vec!["./memories/".into()]),
            grants: ContextGrants::default(),
            sort: None,
            exclude: Vec::new(),
            description: Some("Use the read and write tools to access.".into()),
        }],
    );
}

/// `description` on a bare path fails — it's group-only, like `create` and `sort`.
#[test]
fn context_description_on_bare_path_fails() {
    let err = super::parse(
        Path::new("bad.toml"),
        "[context]\n\"./notes.md\" = { description = \"how to use\" }\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::ContextBarePathGrant(_, "description"))
    ));
}

/// A labeled group's `exclude` — paths pruned from the directory expansion.
#[test]
fn parses_context_group_with_exclude() {
    let role = parse_str(
        r#"
            [context]
            Memories = { path = "./memories/", exclude = ["MEMORY.md", "archive"] }
            "#,
    );
    assert_eq!(
        role.context,
        vec![ContextEntry {
            heading: Some("Memories".into()),
            source: ContextSource::Files(vec!["./memories/".into()]),
            grants: ContextGrants::default(),
            sort: None,
            exclude: vec!["MEMORY.md".into(), "archive".into()],
            description: None,
        }],
    );
}

/// `exclude` on a bare path fails — it filters a directory expansion, and a bare path is
/// one file; group-only like `create`, `sort`, and `description`.
#[test]
fn context_exclude_on_bare_path_fails() {
    let err = super::parse(
        Path::new("bad.toml"),
        "[context]\n\"./notes.md\" = { exclude = [\"x.md\"] }\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::ContextBarePathGrant(_, "exclude"))
    ));
}

/// `exclude` beside `exec` fails — command stdout has no files to prune.
#[test]
fn context_exclude_on_exec_fails() {
    let err = super::parse(
        Path::new("bad.toml"),
        "[context]\nDate = { exec = \"date\", exclude = [\"x.md\"] }\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::ContextExecConflict(_))
    ));
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
            exclude: Vec::new(),
            description: None,
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

/// A misspelled key in a `[context]` table fails the file and is named, rather than
/// being ignored or reported as matching neither spelling (docs/context.md).
#[test]
fn context_table_names_an_unknown_key() {
    let error = super::parse(
        Path::new("bad.toml"),
        "[context]\nNotes = { pth = \"notes/\" }\n",
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("unknown field `pth`"), "{error}");
}
