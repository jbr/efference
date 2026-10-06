//! Tests for the document as a whole, and its non-grant tables.

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

/// The `[env]` table's inline-table form (docs/env.md). Omission is inherit, not
/// "false", which is what lets a user entry compose with a built-in default
/// instead of replacing it wholesale.
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
            strip: Some(false),
            ..Default::default()
        }
    );
    assert_eq!(
        config.env["EFFERENCE_*"],
        crate::env_table::EnvEntry {
            mask: None,
            strip: Some(true),
            ..Default::default()
        }
    );
    assert_eq!(
        config.env["RUST_LOG"],
        crate::env_table::EnvEntry::default()
    );
}

/// The value forms (docs/env.md): a bare string names the value and is
/// scheme-sniffed, `{ value = … }` forces a literal, `{ cmd = … }` is the
/// resolution primitive. Parsing keeps the three distinguishable; nothing runs
/// here.
#[test]
fn the_env_table_parses_the_value_forms() {
    let config = super::parse(
        Path::new("ok.toml"),
        "[env]\n\
         SOME_KEY = \"op://Private/Thing/credential\"\n\
         NOTE = { value = \"op://Private/Thing/credential\" }\n\
         OTHER = { cmd = \"security find-generic-password -s foo -w\" }\n",
    )
    .unwrap();
    assert_eq!(
        config.env["SOME_KEY"].source(),
        Some(crate::env_table::Source::Command {
            command: "op read 'op://Private/Thing/credential'".into(),
            reference: "op://Private/Thing/credential".into(),
        })
    );
    // `{ value = … }` is the escape hatch: a literal even for a registered scheme.
    assert_eq!(
        config.env["NOTE"].source(),
        Some(crate::env_table::Source::Literal(
            "op://Private/Thing/credential".into()
        ))
    );
    assert!(matches!(
        config.env["OTHER"].source(),
        Some(crate::env_table::Source::Command { .. })
    ));
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

/// `[transport] stream-ceiling` is seconds, like `[bash]`'s knobs, and absent
/// means the generous default rather than "no ceiling" (docs/transport.md).
#[test]
fn parses_the_transport_stream_ceiling() {
    let role = parse_str(
        r#"
            [transport]
            stream-ceiling = 900
            "#,
    );
    assert_eq!(role.transport.stream_ceiling, Duration::from_secs(900));
    assert_eq!(
        parse_str("").transport.stream_ceiling,
        crate::api::DEFAULT_STREAM_CEILING
    );
}

/// `[transport] keepalive-interval` is the second transport knob: how long the
/// cached prefix may go untouched at idle before the ping (docs/transport.md).
/// **Zero is the off switch** — the honest spelling, rather than a separate
/// boolean that could disagree with the interval beside it.
#[test]
fn parses_the_transport_keepalive_interval() {
    let role = parse_str(
        r#"
            [transport]
            keepalive-interval = 45
            "#,
    );
    assert_eq!(
        role.transport.keepalive_interval,
        Some(Duration::from_secs(45))
    );
    assert_eq!(parse_str("").transport.keepalive_interval, None);
    assert!(
        parse_str("[transport]\nkeepalive-interval = 0\n")
            .transport
            .keepalive_interval
            .is_some_and(|interval| interval.is_zero())
    );
}

/// `[transport] effort` is the third transport knob and the only one that reaches
/// the request body: the level the model spends per turn (docs/transport.md).
/// Absent means the field is left off the wire entirely, which is the provider's
/// own default — distinct from any level this could name. The value parses as
/// the configured string verbatim: the vocabulary is the model's metadata, so
/// any spelling loads, and the boot check (`ModelCapabilities::check_effort`)
/// is what refuses one the model doesn't declare.
#[test]
fn parses_the_transport_effort() {
    let role = parse_str(
        r#"
            [transport]
            effort = "xhigh"
            "#,
    );
    assert_eq!(role.transport.effort.as_deref(), Some("xhigh"));
    assert_eq!(parse_str("").transport.effort, None);

    // No parse-time vocabulary: a level nobody knows yet loads as given and
    // gets its answer from the model's metadata at boot.
    let role = parse_str(
        r#"
            [transport]
            effort = "extreme"
            "#,
    );
    assert_eq!(role.transport.effort.as_deref(), Some("extreme"));
}

/// `first-turn` is the kickoff's text (docs/context.md); empty is refused, since
/// omitting the key is how a session waits for the operator instead.
#[test]
fn first_turn_parses_and_an_empty_one_is_refused() {
    let role = parse_str(r#"first-turn = "Greet the operator.""#);
    assert_eq!(role.first_turn.as_deref(), Some("Greet the operator."));
    assert!(parse_str("").first_turn.is_none());
    let err = crate::config::Config::from_toml(r#"first-turn = "  ""#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("`first-turn` is empty"), "{err}");
}
