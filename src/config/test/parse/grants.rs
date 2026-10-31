//! Tests for what a session is permitted: commands and paths (tools are `tools.rs`).

use super::*;

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

/// The soundness gate: a `[commands]` key that isn't a single simple command fails the
/// whole file — no silent skip, no partial recovery.
#[test]
fn non_simple_command_key_fails_the_file() {
    let err = parse(Path::new("bad.toml"), "[commands]\n\"git | rm\" = true\n").unwrap_err();
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
    let err = parse(Path::new("bad.toml"), "[commands]\ncargo = \"maybe\"\n").unwrap_err();
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

/// A misspelled permission in a `[paths]` table fails the file and is named
/// (docs/permissions.md).
#[test]
fn path_grant_table_names_an_unknown_key() {
    let error = parse(Path::new("bad.toml"), "[paths]\n\".\" = { raed = true }\n")
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown field `raed`"), "{error}");
}
