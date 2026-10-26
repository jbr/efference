//! Adding a connection whose credential is an API key: where the key comes
//! from, what the check does with it, and whether it is kept in the store.

use super::*;

/// The first-run path with a key already in the shell: no secret prompt, no
/// model question — the new-session form names the current model — and a file
/// whose bytes are pinned.
#[tokio::test]
async fn first_run_with_key_in_environment() {
    let probe = Table {
        env: BTreeMap::from([("ANTHROPIC_API_KEY".to_string(), "sk-real".to_string())]),
        accepted_keys: strings(&["sk-real"]),
        known_models: strings(&["claude-sonnet-4-5", "claude-haiku-4-5"]),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("Anthropic"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type(""),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("ANTHROPIC_API_KEY is set in this shell"));
    assert!(script.said("accepted the key (test key)"));
    assert!(
        !script
            .transcript
            .iter()
            .any(|line| line.starts_with("secret:"))
    );
    assert_eq!(draft.decided_default(DefaultRole::Spotter), None);
    assert_eq!(
        render("", &draft).unwrap(),
        r#"#:schema https://efference.net/schemas/user.v1.json
# The efference user configuration, written by `efference setup`.
# Connections name a provider and where its credential comes from; the
# credential itself is never in this file. Sessions select a model as
# <connection>/<native model>; [defaults] names the spotter.
# Reference: docs/operating.md.

[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }
"#
    );
    let parsed = crate::inference::InferenceConfig::from_toml(&render("", &draft).unwrap());
    assert!(parsed.is_ok());
}

/// A key not in the shell can be pasted for the check alone: the probe sees it,
/// the file never does.
#[tokio::test]
async fn pasted_key_is_checked_and_not_recorded() {
    let probe = Table {
        accepted_keys: strings(&["sk-or-pasted"]),
        known_models: strings(&["anthropic/claude-sonnet-4-5", "anthropic/claude-haiku-4-5"]),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("OpenRouter"),
            Answer::Type("router"),
            Answer::Pick("An environment variable"),
            Answer::Type("MY_ROUTER_KEY"),
            Answer::Yes,
            Answer::Secret("sk-or-pasted"),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("MY_ROUTER_KEY is not set in this shell"));
    assert_eq!(*probe.seen_keys.borrow(), strings(&["sk-or-pasted"]));
    let text = render("", &draft).unwrap();
    assert!(!text.contains("sk-or-pasted"));
    assert!(text.contains(r#"[connections.router]"#));
    assert!(text.contains(r#"auth = { kind = "env", key = "MY_ROUTER_KEY" }"#));
    // A first run ends with no default model: the new-session form names the
    // current one.
    assert!(!text.contains("\n[defaults]"), "{text}");
}

/// A rejected key is said, and declining to keep the connection cancels.
#[tokio::test]
async fn rejected_key_can_cancel() {
    let probe = Table {
        env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "sk-wrong".to_string())]),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("OpenAI"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type(""),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    assert_eq!(draft, Err(Cancelled));
    assert!(script.said("rejected the key: 401: bad key"));
}

/// A rejected key can still record the connection: the first run keeps it and
/// ends with no default model, and the new-session form names the current one.
#[tokio::test]
async fn a_rejected_key_kept_anyway_records_no_default() {
    let probe = Table {
        env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "sk-wrong".to_string())]),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("OpenAI"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type(""),
            // The key is rejected and the connection kept anyway.
            Answer::Yes,
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("rejected the key"));
    assert_eq!(draft.decided_default(DefaultRole::Spotter), None);
    let text = render("", &draft).unwrap();
    assert!(text.contains("[connections.openai]"), "{text}");
    assert!(!text.contains("\n[defaults]"), "{text}");
}

/// The paste path: the key is checked, kept under `inference:<connection>`,
/// and the file records only the kind. The key drives the model menu too.
#[tokio::test]
async fn a_pasted_key_is_kept_in_the_store_and_the_file_names_only_the_kind() {
    let probe = Table {
        accepted_keys: strings(&["sk-pasted"]),
        known_models: strings(&["claude-sonnet-4-5", "claude-haiku-4-5"]),
        secrets: Some(Secrets::in_memory()),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("Anthropic"),
            Answer::Type("work"),
            Answer::Pick("Paste it now, kept in the OS keychain"),
            Answer::Secret("sk-pasted"),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert_eq!(draft.connections[0].1.auth, AuthSource::Keychain);
    assert_eq!(
        probe
            .secrets
            .as_ref()
            .unwrap()
            .get("inference:work")
            .unwrap()
            .as_deref(),
        Some("sk-pasted")
    );
    assert!(script.said("Kept as `inference:work`"));
    let text = render("", &draft).unwrap();
    assert!(text.contains(r#"auth = { kind = "keychain" }"#), "{text}");
    assert!(!text.contains("sk-pasted"));
}

/// A rejected key is not kept unless the person says to record it anyway.
#[tokio::test]
async fn a_rejected_pasted_key_is_not_kept_when_the_person_backs_out() {
    let probe = Table {
        secrets: Some(Secrets::in_memory()),
        ..Default::default()
    };
    let (draft, _) = run_script(
        vec![
            Answer::Pick("Anthropic"),
            Answer::Type("work"),
            Answer::Pick("Paste it now, kept in the OS keychain"),
            Answer::Secret("sk-bad"),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    assert_eq!(draft.unwrap_err(), Cancelled);
    assert_eq!(
        probe
            .secrets
            .as_ref()
            .unwrap()
            .get("inference:work")
            .unwrap(),
        None
    );
}

/// With no store to write, the keychain entry is listed with the reason.
#[test]
fn keychain_is_shown_unavailable_without_a_store() {
    struct Capture(Vec<Choice>);
    impl Prompter for Capture {
        fn say(&mut self, _: Tone, _: &str) {}
        fn select(&mut self, _: &str, choices: &[Choice]) -> Result<usize, Cancelled> {
            self.0 = choices.to_vec();
            Err(Cancelled)
        }
        fn text(&mut self, _: &str, _: Option<&str>) -> Result<String, Cancelled> {
            Ok("x".into())
        }
        fn secret(&mut self, _: &str) -> Result<String, Cancelled> {
            Err(Cancelled)
        }
        fn confirm(&mut self, _: &str, _: bool) -> Result<bool, Cancelled> {
            Err(Cancelled)
        }
    }
    let mut capture = Capture(Vec::new());
    let probe = Table::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    // The first select is the provider menu; the capture cancels there, so
    // drive the key-source question directly.
    let result = runtime.block_on(super::flow::api_key_source(
        &mut capture,
        &probe,
        &Target::named(Provider::Anthropic),
        "anthropic",
        "ANTHROPIC_API_KEY",
    ));
    assert_eq!(result.unwrap_err(), Cancelled);
    let keychain = capture
        .0
        .iter()
        .find(|choice| choice.label.contains("keychain"))
        .unwrap();
    assert_eq!(
        keychain.unavailable.as_deref(),
        Some("no store on this platform")
    );
}

/// A decision-model provider alone is not a complete config: the flow says so
/// and asks for another connection. A first run ends with no default model.
#[tokio::test]
async fn a_typesafe_only_config_asks_for_a_provider_for_the_agent() {
    let probe = Table {
        env: BTreeMap::from([
            ("TYPESAFE_API_KEY".to_string(), "ts-real".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "sk-real".to_string()),
        ]),
        accepted_keys: strings(&["ts-real", "sk-real"]),
        known_models: strings(&["claude-sonnet-4-5"]),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("TypeSafe"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type(""),
            Answer::No,
            Answer::Pick("Anthropic"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type(""),
            Answer::No,
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("serves the permission spotter only"));
    assert_eq!(draft.decided_default(DefaultRole::Spotter), None);
    let text = render("", &draft).unwrap();
    assert!(text.contains("[connections.typesafe]"), "{text}");
    assert!(!text.contains("\n[defaults]"), "{text}");
    assert!(
        crate::inference::InferenceConfig::from_toml(&text).is_ok(),
        "{text}"
    );
}
