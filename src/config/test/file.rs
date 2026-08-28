//! Tests for `file.rs`.

use super::*;

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
    assert!(role.commands.contains_key("git diff"));
    assert!(role.commands.contains_key("cargo"));

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

/// Write-back into a file that already spells stances (docs/permissions.md): the harness's own
/// edit is a **clear** grant, written as the `true` sugar, and it leaves every other entry —
/// including a `deny` carve-out — exactly as the author wrote it. Reloading gives back the same
/// stances, so the round-trip is stance-preserving and the promotion path still means what it
/// meant before the vocabulary existed.
#[test]
fn persisting_a_grant_preserves_neighbouring_stances() {
    let path = std::env::temp_dir().join(format!("efference-stance-{}.toml", std::process::id()));
    std::fs::write(
        &path,
        "[commands]\ngit = \"clear\"\n\"git push\" = \"deny\"  # never from a session\n",
    )
    .unwrap();

    persist_config_change(
        &path,
        &ConfigChange::Command {
            command: "cargo".into(),
        },
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("cargo = true"), "the grant is sugar: {text}");
    assert!(
        text.contains("\"git push\" = \"deny\"  # never from a session"),
        "the carve-out and its comment are untouched: {text}"
    );

    let role = parse(&path, &text).unwrap();
    assert_eq!(role.commands["cargo"], CommandStance::Clear);
    assert_eq!(role.commands["git"], CommandStance::Clear);
    assert_eq!(role.commands["git push"], CommandStance::Deny);

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

/// A connected tool's grant is written in the dotted form the operator reads everywhere
/// else, and the file round-trips back to the wire name the fast-path matches. That
/// round-trip is what keeps the auto-persist echo idempotent: the reloaded file normalizes
/// to the same wire name, so the next diff finds nothing new.
#[test]
fn persist_tool_writes_the_dotted_form_and_round_trips_to_the_wire_name() {
    let path = std::env::temp_dir().join(format!("efference-dotted-{}.toml", std::process::id()));
    std::fs::write(&path, "tools = [\"read\"]\n").unwrap();

    let change = ConfigChange::Tool {
        name: "ferritin__get".into(),
    };
    persist_config_change(&path, &change).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("ferritin.get"), "dotted in the file: {text}");
    assert!(
        !text.contains("ferritin__get"),
        "wire form stays off the page: {text}"
    );

    // The loader resolves it back, so the fast-path still matches one name.
    let role = parse(&path, &text).unwrap();
    assert!(role.tools.contains("ferritin__get"));
    assert!(role.tools.contains("read"));

    // Re-persisting is a no-op: membership is compared normalized, so the entry already
    // present in dotted form is recognized as the same grant.
    persist_config_change(&path, &change).unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        after.matches("ferritin.get").count(),
        1,
        "idempotent: {after}"
    );

    std::fs::remove_file(&path).unwrap();
}

/// The other spelling of the same grant: a hand-written wire-form entry must not gain a
/// second, dotted entry when the harness persists the same grant.
#[test]
fn persist_tool_does_not_duplicate_a_hand_written_wire_form_entry() {
    let path = std::env::temp_dir().join(format!("efference-dotted2-{}.toml", std::process::id()));
    std::fs::write(&path, "tools = [\"ferritin__get\"]\n").unwrap();

    persist_config_change(
        &path,
        &ConfigChange::Tool {
            name: "ferritin__get".into(),
        },
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        !text.contains("ferritin.get"),
        "no second spelling appended: {text}"
    );
    assert_eq!(
        text.matches("ferritin__get").count(),
        1,
        "unchanged: {text}"
    );

    std::fs::remove_file(&path).unwrap();
}
