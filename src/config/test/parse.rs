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
    assert!(role.commands.contains_key("cargo"));
    assert!(role.commands.contains_key("git diff"));
    assert!(role.commands.contains_key("git status"));
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

/// The `[commands]` value vocabulary (docs/permissions.md): the four stance words parse, and
/// the `bool` sugar keeps parsing as the two ends of it — `true` = `clear` (what every config
/// written before the vocabulary means) and `false` = `deny` (the carve-out spelling).
#[test]
fn command_values_parse_as_stances_and_bool_sugar() {
    let role = parse_str(
        r#"
            [commands]
            cargo = true
            npm = false
            git = "clear"
            "git push" = "deny"
            "git rebase" = "watch"
            "git reset" = "gate"
        "#,
    );
    assert_eq!(role.commands["cargo"], CommandStance::Clear);
    assert_eq!(role.commands["npm"], CommandStance::Deny);
    assert_eq!(role.commands["git"], CommandStance::Clear);
    assert_eq!(role.commands["git push"], CommandStance::Deny);
    assert_eq!(role.commands["git rebase"], CommandStance::Watch);
    assert_eq!(role.commands["git reset"], CommandStance::Gate);
}

/// A value that is neither a bool nor one of the four stance words fails the file, like any
/// other malformed entry — the vocabulary is closed, so a typo is loud rather than ignored.
#[test]
fn unknown_command_stance_fails_the_file() {
    let err = super::parse(Path::new("bad.toml"), "[commands]\ncargo = \"maybe\"\n").unwrap_err();
    assert!(matches!(err, LoadError::Parse(..)));
}

/// A multi-token carve-out key lexes like any other key, so the soundness requirement (a key
/// lexes to the same token vector the command is checked against) already admits carve-outs.
#[test]
fn carve_out_keys_pass_the_lexing_gate() {
    let role = parse_str("[commands]\ngit = true\n\"git push --force\" = \"deny\"\n");
    assert_eq!(
        crate::policy::simple_command_tokens("git push --force"),
        Some(vec![
            "git".to_string(),
            "push".to_string(),
            "--force".to_string()
        ])
    );
    assert_eq!(role.commands["git push --force"], CommandStance::Deny);
}

#[test]
fn mcp_servers_parse_into_transports() {
    let config = parse_str(
        r#"
[mcp.servers.github]
command = "gh-mcp-server"
args = ["--stdio"]
env = { GITHUB_TOKEN = "t" }

[mcp.servers.sentry]
url = "https://mcp.example.com/mcp"
"#,
    );
    assert_eq!(config.mcp_servers.len(), 2);
    // BTreeMap: mount order is name order, deterministic.
    let names: Vec<&str> = config.mcp_servers.keys().map(String::as_str).collect();
    assert_eq!(names, ["github", "sentry"]);
    assert!(matches!(
        &config.mcp_servers["github"].transport,
        McpTransport::Stdio { command, args, env }
            if command == "gh-mcp-server" && args == &["--stdio"] && env["GITHUB_TOKEN"] == "t"
    ));
    assert!(matches!(
        &config.mcp_servers["sentry"].transport,
        McpTransport::Http { url } if url == "https://mcp.example.com/mcp"
    ));
    // Required is the default: a declared server that will not mount stops the boot.
    assert!(config.mcp_servers.values().all(|entry| !entry.optional));
}

/// `optional = true` is the opt-out from the fatal default. The default is the
/// strict one deliberately: an opt-*in* to strictness is one nobody remembers to
/// write, so the safe behavior has to be what you get for free.
#[test]
fn a_server_can_opt_out_of_being_required() {
    let config = parse_str(
        r#"
[mcp.servers.required_by_default]
command = "a"

[mcp.servers.explicitly_optional]
command = "b"
optional = true

[mcp.servers.explicitly_required]
url = "https://example.com/mcp"
optional = false
"#,
    );
    assert!(!config.mcp_servers["required_by_default"].optional);
    assert!(config.mcp_servers["explicitly_optional"].optional);
    assert!(!config.mcp_servers["explicitly_required"].optional);
}

/// The stanza's shape picks the transport, so it must commit to exactly one.
#[test]
fn mcp_server_with_both_or_neither_transport_fails_the_file() {
    for text in [
        "[mcp.servers.x]\ncommand = \"a\"\nurl = \"https://b\"\n",
        "[mcp.servers.x]\nargs = [\"--stdio\"]\n",
    ] {
        let err = super::parse(Path::new("bad.toml"), text).unwrap_err();
        assert!(matches!(
            err,
            LoadError::Validate(_, ValidationError::McpServerTransport(_))
        ));
    }
}

/// `args`/`env` parameterize a spawned subprocess; an HTTP server is not spawned.
#[test]
fn mcp_http_server_with_spawn_keys_fails_the_file() {
    let err = super::parse(
        Path::new("bad.toml"),
        "[mcp.servers.x]\nurl = \"https://b\"\nenv = { A = \"1\" }\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::McpServerHttpExtras(_))
    ));
}

/// The server name prefixes every advertised tool on the wire (`<server>__<tool>`),
/// so it must fit the API's tool-name alphabet.
#[test]
fn mcp_server_name_outside_the_tool_alphabet_fails_the_file() {
    let err = super::parse(
        Path::new("bad.toml"),
        "[mcp.servers.\"my server!\"]\ncommand = \"a\"\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::McpServerName(_))
    ));
}

/// `__` is the namespace separator itself. A server name containing one would move the
/// boundary every direction of the mapping resolves as the *first* `__`, so it is
/// rejected even though `_` is otherwise a legal character.
#[test]
fn mcp_server_name_containing_the_separator_fails_the_file() {
    let err = super::parse(
        Path::new("bad.toml"),
        "[mcp.servers.my__server]\ncommand = \"a\"\n",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        LoadError::Validate(_, ValidationError::McpServerName(_))
    ));
    // A single underscore is still fine — only the doubled form is the separator.
    let ok = super::parse(
        Path::new("ok.toml"),
        "[mcp.servers.my_server]\ncommand = \"a\"\n",
    )
    .unwrap();
    assert!(ok.mcp_servers.contains_key("my_server"));
}

/// The TUI shows `ferritin.get`, so that is what an operator types — but the grant is
/// matched against the wire name. The dotted form is resolved at validate and never
/// stored, so the fast-path still sees exactly one name.
#[test]
fn a_dotted_tool_grant_normalizes_to_the_wire_name() {
    let config = super::parse(
        Path::new("ok.toml"),
        "tools = [\"ferritin.get\", \"read\", \"ferritin.admin.tools.list\"]\n",
    )
    .unwrap();
    assert!(config.tools.contains("ferritin__get"));
    // Only the first dot is the boundary: a server's own tool name may contain dots
    // (the MCP spec permits them), and those stay put.
    assert!(config.tools.contains("ferritin__admin.tools.list"));
    // An undotted entry — every embedded tool name — passes through untouched.
    assert!(config.tools.contains("read"));
    // The dotted form is an alias, not a second key.
    assert!(!config.tools.contains("ferritin.get"));
}

/// The `[env]` table parses only the inline-table form (docs/env.md). Omission is
/// inherit, not "false", which is what lets a user entry compose with a built-in
/// default instead of replacing it wholesale.
#[test]
fn the_env_table_parses_the_inline_table_form() {
    let config = super::parse(
        Path::new("ok.toml"),
        "[env]\n\
         \"*_KEY\" = { mask = true, strip = false }\n\
         \"EFFERENCE_*\" = { strip = true }\n\
         RUST_LOG = {}\n",
    )
    .unwrap();
    assert_eq!(
        config.env["*_KEY"],
        crate::env_table::EnvEntry {
            mask: Some(true),
            strip: Some(false)
        }
    );
    assert_eq!(
        config.env["EFFERENCE_*"],
        crate::env_table::EnvEntry {
            mask: None,
            strip: Some(true)
        }
    );
    assert_eq!(
        config.env["RUST_LOG"],
        crate::env_table::EnvEntry::default()
    );
}

/// A bare `= false` shorthand cannot say *which* of the two questions it answers
/// (docs/env.md), so it is a parse error rather than a guess. Same for a typo'd key.
#[test]
fn the_env_table_rejects_the_bare_bool_shorthand_and_unknown_keys() {
    assert!(super::parse(Path::new("bad.toml"), "[env]\n\"*_KEY\" = false\n").is_err());
    assert!(super::parse(Path::new("bad.toml"), "[env]\n\"*_KEY\" = true\n").is_err());
    assert!(
        super::parse(Path::new("bad.toml"), "[env]\nA_KEY = { msak = true }\n").is_err(),
        "deny_unknown_fields makes a typo loud"
    );
}
