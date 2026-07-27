use super::*;

/// Load → apply → sync. The persistence tests below predate [`ConfigFile`]; they exercise the
/// same edit path through a fresh handle, which is also the honest read of what they cover
/// (the on-disk result of one change), independent of the held-document invariant.
fn persist_config_change(path: &Path, change: &ConfigChange) -> std::io::Result<()> {
    let mut file = ConfigFile::load(path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    file.apply(change)
}

fn parse_str(text: &str) -> Config {
    super::parse(Path::new("test.toml"), text).expect("valid config")
}

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

// --- [context] preamble builder (docs/context.md) ---

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

#[test]
fn explicit_missing_config_is_an_io_error() {
    let missing =
        std::env::temp_dir().join(format!("efference-no-such-{}.toml", std::process::id()));
    let _ = std::fs::remove_file(&missing);
    let err = load(Some(&missing)).unwrap_err();
    assert!(
        matches!(err, LoadError::Io(_, _)),
        "explicit path must exist"
    );
}

#[test]
fn explicit_present_config_resolves_with_its_path() {
    let path = std::env::temp_dir().join(format!("efference-config-{}.toml", std::process::id()));
    std::fs::write(&path, r#"scope = "hi""#).unwrap();
    let loaded = load(Some(&path)).unwrap();
    assert_eq!(loaded.role.scope.as_deref(), Some("hi"));
    assert_eq!(loaded.source, ConfigSource::Path(path.clone()));
    std::fs::remove_file(&path).unwrap();
}

/// Persisting a command change sets `[commands][key] = true`, preserves unrelated comments
/// and formatting, round-trips back through the loader, and is idempotent on re-persist.
#[test]
fn persist_command_sets_key_preserves_comments_and_round_trips() {
    let path = std::env::temp_dir().join(format!("efference-persist-{}.toml", std::process::id()));
    std::fs::write(
        &path,
        "# my config\nscope = \"do the thing\"  # the mission\n\n[commands]\ncargo = true\n",
    )
    .unwrap();

    let change = ConfigChange::Command {
        command: "git diff".into(),
    };
    persist_config_change(&path, &change).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# my config"), "comment preserved: {text}");
    assert!(
        text.contains("# the mission"),
        "inline comment preserved: {text}"
    );
    assert!(text.contains(r#""git diff" = true"#), "key set: {text}");

    // Round-trips back through the loader into the command set.
    let role = parse(&path, &text).unwrap();
    assert!(role.commands.contains("git diff"));
    assert!(role.commands.contains("cargo"));

    // Re-persisting the same change is a no-op — no duplicate key.
    persist_config_change(&path, &change).unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        after.matches(r#""git diff" = true"#).count(),
        1,
        "idempotent: {after}"
    );

    std::fs::remove_file(&path).unwrap();
}

/// The held document round-trips to its original bytes. This is the property the whole
/// revert path rests on (docs/permissions.md): reverting writes the
/// held document out, so if `to_string` were lossy, every revert would silently reformat the
/// author's file — including the schema header comment the parse-error message tells the
/// model to go read.
#[test]
fn an_untouched_document_syncs_back_byte_identically() {
    let path =
        std::env::temp_dir().join(format!("efference-roundtrip-{}.toml", std::process::id()));
    // Comments, alignment, blank lines, key order, quoted and bare keys, inline tables.
    let original = "# efference config — schema in this header\n\
             scope = \"stay in src/\"   # the mission\n\
             \n\
             watch-for = [\n  \"network egress\",\n]\n\
             \n\
             [commands]\n\
             cargo   = true\n\
             \"git diff\" = true   # sub-verb\n\
             \n\
             # filesystem\n\
             [paths]\n\
             \".\" = true\n\
             \"/refs\" = { read = true }\n";
    std::fs::write(&path, original).unwrap();

    let file = ConfigFile::load(&path).unwrap();
    std::fs::write(&path, "clobbered").unwrap();
    file.sync().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        original,
        "sync of an untouched document reproduces the file byte for byte"
    );

    std::fs::remove_file(&path).unwrap();
}

/// [`ConfigFile::refresh`] is the two-outcome rule (docs/permissions.md):
/// a file that parses is **adopted**, one that doesn't is **reverted** to the last content that
/// parsed. Also covers the unreadable case (deleted out from under us), which restores the file
/// rather than leaving the session with no config on disk.
#[test]
fn refresh_adopts_what_parses_and_reverts_what_does_not() {
    let path = std::env::temp_dir().join(format!("efference-refresh-{}.toml", std::process::id()));
    let good = "# keep me\nscope = \"first\"\n[commands]\ncargo = true\n";
    std::fs::write(&path, good).unwrap();
    let mut file = ConfigFile::load(&path).unwrap();

    // A valid edit is adopted: the new config is returned and becomes the held one.
    let edited = "# keep me\nscope = \"second\"\n[commands]\ncargo = true\n";
    std::fs::write(&path, edited).unwrap();
    match file.refresh() {
        Refreshed::Adopted(config) => assert_eq!(config.scope.as_deref(), Some("second")),
        other => panic!("expected Adopted, got {other:?}"),
    }
    assert_eq!(file.config().scope.as_deref(), Some("second"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited, "untouched");

    // The footgun: `[scope]` as a table where the schema wants a string. Reverted to the last
    // content that parsed — which is the *adopted* edit, not the original file.
    std::fs::write(&path, "[scope]\ndefault = \"read\"\n").unwrap();
    match file.refresh() {
        Refreshed::Reverted { error } => assert!(
            error.contains("scope"),
            "the reason names the offending key: {error}"
        ),
        other => panic!("expected Reverted, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        edited,
        "reverted to the last content that parsed, comments intact"
    );
    assert_eq!(
        file.config().scope.as_deref(),
        Some("second"),
        "the rejected edit never reaches policy"
    );

    // A file deleted out from under the session is restored the same way.
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(file.refresh(), Refreshed::Reverted { .. }));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);

    std::fs::remove_file(&path).unwrap();
}

/// Persisting into an **empty** file vivifies the missing sections — the case the gating
/// test (which edited an existing `[commands]`) didn't cover. Tool grants append to the
/// `tools` array and dedup on membership; path grants render the sugar / inline perms.
#[test]
fn persist_into_empty_file_vivifies_sections() {
    let path = std::env::temp_dir().join(format!(
        "efference-persist-empty-{}.toml",
        std::process::id()
    ));
    std::fs::write(&path, "").unwrap();

    persist_config_change(
        &path,
        &ConfigChange::Tool {
            name: "read".into(),
        },
    )
    .unwrap();
    persist_config_change(
        &path,
        &ConfigChange::Tool {
            name: "read".into(),
        },
    )
    .unwrap();
    persist_config_change(
        &path,
        &ConfigChange::Path {
            path: "/refs".into(),
            read: true,
            write: false,
        },
    )
    .unwrap();
    persist_config_change(
        &path,
        &ConfigChange::Path {
            path: ".".into(),
            read: true,
            write: true,
        },
    )
    .unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    let role = parse(&path, &text).unwrap();
    // The tool grant landed once (membership dedup, not last-write-wins).
    assert_eq!(
        role.tools.iter().filter(|t| *t == "read").count(),
        1,
        "{text}"
    );
    // Path sugar + inline perms both decode back.
    assert_eq!(
        role.paths["."],
        Perms {
            read: true,
            write: true
        },
        "{text}"
    );
    assert_eq!(
        role.paths["/refs"],
        Perms {
            read: true,
            write: false
        },
        "{text}"
    );

    std::fs::remove_file(&path).unwrap();
}

/// A `Scope` change replaces the scalar in place (preserving its inline comment) when present,
/// and inserts it when absent — the narrowing edit append-only KDL couldn't express.
#[test]
fn persist_scope_replaces_in_place_then_inserts_when_absent() {
    let path = std::env::temp_dir().join(format!(
        "efference-persist-scope-{}.toml",
        std::process::id()
    ));

    // Replace in place: the inline comment on the scope line survives.
    std::fs::write(
        &path,
        "scope = \"old\"  # the perimeter\n[commands]\ncargo = true\n",
    )
    .unwrap();
    persist_config_change(
        &path,
        &ConfigChange::Scope {
            text: "narrowed".into(),
        },
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains(r#"scope = "narrowed""#),
        "scope replaced: {text}"
    );
    assert!(
        text.contains("# the perimeter"),
        "inline comment preserved: {text}"
    );
    assert!(
        text.contains("cargo = true"),
        "unrelated section intact: {text}"
    );
    assert_eq!(
        parse(&path, &text).unwrap().scope.as_deref(),
        Some("narrowed")
    );

    // Insert when absent: a file with no scope gains one.
    std::fs::write(&path, "[commands]\ncargo = true\n").unwrap();
    persist_config_change(
        &path,
        &ConfigChange::Scope {
            text: "fresh".into(),
        },
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        parse(&path, &text).unwrap().scope.as_deref(),
        Some("fresh"),
        "{text}"
    );

    std::fs::remove_file(&path).unwrap();
}

// --- diff_changes: the fs-watch initiator's file-vs-effective delta ---

/// New keys the file adds over the effective config surface as additive changes — one per slot,
/// in deterministic (sorted) order; a scope change replaces the scalar.
#[test]
fn diff_emits_additive_changes_for_what_the_file_adds() {
    let from = parse_str("scope = \"old\"\n[commands]\ncargo = true\n");
    let to = parse_str(
        "scope = \"new\"\ntools = [\"read\"]\n\
             [commands]\ncargo = true\n\"git diff\" = true\n[paths]\n\".\" = true\n",
    );
    let changes = diff_changes(&from, &to);
    assert_eq!(
        changes,
        vec![
            ConfigChange::Scope { text: "new".into() },
            ConfigChange::Command {
                command: "git diff".into()
            },
            ConfigChange::Tool {
                name: "read".into()
            },
            ConfigChange::Path {
                path: ".".into(),
                read: true,
                write: true
            },
        ],
    );
}

/// Diffing a config against itself is empty — the property that makes the harness's own
/// auto-persist write-back a harmless no-op echo (the file already matches effective).
#[test]
fn diff_against_self_is_empty() {
    let cfg = parse_str("scope = \"s\"\ntools = [\"read\"]\n[commands]\ncargo = true\n");
    assert!(diff_changes(&cfg, &cfg).is_empty());
}

/// A *changed* path perm surfaces (write added); a removed key does **not** (the additive fold
/// can't express removal — tightening waits for the next boot, which reloads the file as base).
#[test]
fn diff_emits_perm_changes_but_not_removals() {
    let from =
        parse_str("[commands]\ncargo = true\nnpm = true\n[paths]\n\".\" = { read = true }\n");
    let to = parse_str("[commands]\ncargo = true\n[paths]\n\".\" = true\n");
    let changes = diff_changes(&from, &to);
    // `.` gained write — emitted. `npm` was removed — not emitted (additive only).
    assert_eq!(
        changes,
        vec![ConfigChange::Path {
            path: ".".into(),
            read: true,
            write: true
        }],
    );
}
