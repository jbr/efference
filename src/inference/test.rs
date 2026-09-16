use super::*;

fn environment(values: &[(&str, &str)]) -> EnvSnapshot {
    EnvSnapshot::new(
        values
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect(),
    )
    .unwrap()
}

fn registry(toml: &str, environment: EnvSnapshot) -> InferenceRegistry {
    registry_with_secrets(toml, environment, &Arc::new(Secrets::in_memory()))
}

/// The same, against a store a test has already put something in.
fn registry_with_secrets(
    toml: &str,
    environment: EnvSnapshot,
    secrets: &Arc<Secrets>,
) -> InferenceRegistry {
    let config = InferenceConfig::from_toml(toml).unwrap();
    InferenceRegistry::from_config(config, Path::new("/host"), environment, secrets).unwrap()
}

#[test]
fn qualified_roles_can_use_two_accounts_of_one_provider() {
    let registry = registry(
        r#"
[defaults]
model = "work/anthropic/main"
spotter = "personal/anthropic/cheap"

[connections.work]
provider = "openrouter"
auth = { kind = "env", key = "WORK_KEY" }

[connections.personal]
provider = "openrouter"
auth = { kind = "env", key = "PERSONAL_KEY" }
"#,
        environment(&[
            ("WORK_KEY", "work-secret"),
            ("PERSONAL_KEY", "personal-secret"),
        ]),
    );

    let selected = registry.resolve(None, None, None).unwrap();
    assert_eq!(selected.main().0.connection, "work");
    assert_eq!(selected.main().0.model, "anthropic/main");
    assert_eq!(selected.spotter().0.connection, "personal");
    assert_eq!(selected.spotter().0.model, "anthropic/cheap");
    assert!(selected.triage().is_none());
}

#[test]
fn unqualified_native_model_with_slashes_uses_main_default_connection() {
    let registry = registry(
        r#"
[defaults]
model = "router/anthropic/default"

[connections.router]
provider = "openrouter"
auth = { kind = "env", key = "ROUTER_KEY" }
"#,
        environment(&[("ROUTER_KEY", "router-secret")]),
    );

    let selected = registry
        .resolve(Some("vendor/claude-sonnet-4-5"), None, None)
        .unwrap();
    assert_eq!(selected.main().0.connection, "router");
    assert_eq!(selected.main().0.model, "vendor/claude-sonnet-4-5");
}

#[test]
fn unavailable_connection_does_not_poison_ready_connection() {
    let registry = registry(
        r#"
[defaults]
model = "ready/main"

[connections.ready]
provider = "openrouter"
auth = { kind = "env", key = "READY_KEY" }

[connections.missing]
provider = "anthropic"
auth = { kind = "env", key = "MISSING_KEY" }
"#,
        environment(&[("READY_KEY", "ready-secret")]),
    );

    assert!(registry.resolve(None, None, None).is_ok());
    let error = match registry.resolve(Some("missing/claude"), None, None) {
        Ok(_) => panic!("missing connection unexpectedly resolved"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("missing"), "{error}");
    assert!(error.contains("MISSING_KEY"), "{error}");
}

#[test]
fn own_triage_rejects_a_foreign_dialect_before_boot() {
    let registry = registry(
        r#"
[defaults]
model = "messages/claude"

[connections.messages]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }

[connections.responses]
provider = "openai"
auth = { kind = "env", key = "OPENAI_KEY" }
"#,
        environment(&[
            ("ANTHROPIC_KEY", "anthropic-secret"),
            ("OPENAI_KEY", "openai-secret"),
        ]),
    );

    let error = match registry.resolve(
        None,
        None,
        Some(TriageModel {
            model: "responses/gpt-5.6".to_string(),
            thinking: false,
            auto_choice: false,
            effort: None,
        }),
    ) {
        Ok(_) => panic!("cross-dialect triage unexpectedly resolved"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("dialects differ"), "{error}");
}

#[test]
fn config_defaults_must_be_qualified() {
    let config = InferenceConfig::from_toml(
        r#"
[defaults]
model = "claude"

[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    )
    .unwrap();
    let error = match InferenceRegistry::from_config(
        config,
        Path::new("/host"),
        environment(&[("ANTHROPIC_KEY", "secret")]),
        &Arc::new(Secrets::in_memory()),
    ) {
        Ok(_) => panic!("unqualified default unexpectedly loaded"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("qualified"), "{error}");
}

/// A built-in provider name is always a connection prefix, configured or not:
/// `anthropic/…` on a host with no `anthropic` connection names the missing
/// connection rather than resolving as a native id on the default connection.
#[test]
fn reserved_prefixes_name_connections_even_when_unconfigured() {
    let registry = registry(
        r#"
[defaults]
model = "router/anthropic/default"

[connections.router]
provider = "openrouter"
auth = { kind = "env", key = "ROUTER_KEY" }
"#,
        environment(&[("ROUTER_KEY", "router-secret")]),
    );
    let error = match registry.resolve(Some("anthropic/claude-sonnet-4-5"), None, None) {
        Ok(_) => panic!("reserved prefix unexpectedly resolved on the default connection"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("\"anthropic\""), "{error}");
    assert!(error.contains("not configured"), "{error}");
    // Fully qualifying the configured connection always works.
    let selected = registry
        .resolve(Some("router/anthropic/claude-sonnet-4-5"), None, None)
        .unwrap();
    assert_eq!(selected.main().0.connection, "router");
    assert_eq!(selected.main().0.model, "anthropic/claude-sonnet-4-5");
    assert_eq!(selected.main().0.provider, Provider::OpenRouter);
}

/// A native id that is exactly a connection name must be written qualified; a
/// connection named with a slash is a config error.
#[test]
fn connection_name_collisions_and_slashes_are_rejected() {
    let registry = registry(
        r#"
[defaults]
model = "main/claude"

[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
        environment(&[("ANTHROPIC_KEY", "secret")]),
    );
    let error = match registry.resolve(Some("main"), None, None) {
        Ok(_) => panic!("bare connection name unexpectedly resolved"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("no native model"), "{error}");

    let config = InferenceConfig::from_toml(
        r#"
[connections."a/b"]
provider = "anthropic"
auth = { kind = "env", key = "K" }
"#,
    )
    .unwrap();
    let error = match InferenceRegistry::from_config(
        config,
        Path::new("/host"),
        environment(&[]),
        &Arc::new(Secrets::in_memory()),
    ) {
        Ok(_) => panic!("slashed connection name unexpectedly loaded"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("no '/'"), "{error}");
}

/// A disabled connection stays in the catalog but is never resolved, and a
/// [defaults] entry naming it fails at load, identical to naming a connection
/// that was never configured (docs/operating.md).
#[test]
fn a_disabled_connection_is_never_a_candidate() {
    let config = InferenceConfig::from_toml(
        r#"
[defaults]
model = "main/claude"

[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
disabled = true
"#,
    )
    .unwrap();
    let error = match InferenceRegistry::from_config(
        config,
        Path::new("/host"),
        environment(&[("ANTHROPIC_KEY", "secret")]),
        &Arc::new(Secrets::in_memory()),
    ) {
        Ok(_) => panic!("[defaults].model naming a disabled connection unexpectedly loaded"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("\"main\""), "{error}");
    assert!(
        error.contains("is disabled"),
        "the operator turned it off; reporting it as unconfigured sends them looking \
         for a connection that is right there in the file: {error}"
    );
}

/// A disabled connection with no [defaults] naming it loads fine; resolving a
/// role against it explicitly still fails, by the ordinary unavailable-
/// credential path.
#[test]
fn a_disabled_connection_can_still_be_named_explicitly_and_fails_to_resolve() {
    let registry = registry(
        r#"
[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
disabled = true
"#,
        environment(&[("ANTHROPIC_KEY", "secret")]),
    );
    let error = match registry.resolve(Some("main/claude"), None, None) {
        Ok(_) => panic!("a disabled connection unexpectedly resolved"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("unavailable"), "{error}");
    assert!(error.contains("disabled"), "{error}");
}

/// Explicit triage is own-client mode even when it names the main model; only an
/// omitted triage (with no configured default) shares the main client.
#[test]
fn explicit_triage_on_the_main_model_is_its_own_client() {
    let registry = registry(
        r#"
[defaults]
model = "main/claude"

[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
        environment(&[("ANTHROPIC_KEY", "secret")]),
    );
    let shared = registry.resolve(None, None, None).unwrap();
    assert!(shared.triage().is_none());
    assert!(shared.identity().triage.shared);

    let own = registry
        .resolve(
            None,
            None,
            Some(TriageModel {
                model: "claude".to_string(),
                thinking: true,
                auto_choice: false,
                effort: None,
            }),
        )
        .unwrap();
    let (selection, _, options) = own.triage().unwrap();
    assert_eq!(selection.model, "claude");
    assert!(options.thinking);
    assert!(!own.identity().triage.shared);
}

/// `codex-cache` reads the host's Codex CLI cache once, from the host's HOME;
/// an `env` credential on the Codex provider is unusable without an account id,
/// and says so as that connection's status rather than a load failure.
#[test]
fn codex_cache_loads_from_the_host_home_and_env_needs_an_account() {
    let root = std::env::temp_dir().join(format!("efference-inference-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home/.codex")).unwrap();
    std::fs::write(
        root.join("home/.codex/auth.json"),
        r#"{"tokens": {"access_token": "cached-token", "account_id": "acct"}}"#,
    )
    .unwrap();
    let registry = registry(
        r#"
[defaults]
model = "codex/gpt-5.6"

[connections.codex]
provider = "openai-codex"
auth = { kind = "codex-cache" }

[connections.bare]
provider = "openai-codex"
auth = { kind = "env", key = "BARE_KEY" }
"#,
        environment(&[
            ("HOME", root.join("home").to_str().unwrap()),
            ("BARE_KEY", "bare-token"),
        ]),
    );
    let selected = registry.resolve(None, None, None).unwrap();
    let (_, endpoint) = selected.main();
    assert_eq!(endpoint.auth_value, "Bearer cached-token");
    assert_eq!(
        endpoint.extra_headers,
        vec![("ChatGPT-Account-ID".to_string(), "acct".to_string())]
    );
    let error = match registry.resolve(Some("bare/gpt-5.6"), None, None) {
        Ok(_) => panic!("codex env credential without an account unexpectedly resolved"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("account ID"), "{error}");
    std::fs::remove_dir_all(&root).unwrap();
}

/// The role identity serializes the provider by its configured spelling and
/// repeats the main selection for a shared triage.
#[test]
fn identity_serializes_selections_by_name() {
    let registry = registry(
        r#"
[defaults]
model = "work/gpt-5.6"
spotter = "work/gpt-5.6-luna"

[connections.work]
provider = "openai"
auth = { kind = "env", key = "WORK_KEY" }
"#,
        environment(&[("WORK_KEY", "work-secret")]),
    );
    let identity = registry.resolve(None, None, None).unwrap().identity();
    assert_eq!(
        serde_json::to_value(&identity).unwrap(),
        serde_json::json!({
            "main": {"connection": "work", "provider": "openai", "model": "gpt-5.6"},
            "spotter": {"connection": "work", "provider": "openai", "model": "gpt-5.6-luna"},
            "triage": {"shared": true, "connection": "work", "provider": "openai", "model": "gpt-5.6"},
        })
    );
}

/// A host does not start half-working (docs/operating.md): a configured
/// connection whose credential will not load is reported at launch, rather than
/// as a failure on every session that happens to touch that provider.
#[test]
fn a_connection_that_cannot_authenticate_is_reported_for_launch() {
    let registry = registry(
        r#"
[connections.working]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }

[connections.broken]
provider = "openrouter"
auth = { kind = "env", key = "NEVER_SET" }
"#,
        environment(&[("ANTHROPIC_KEY", "secret")]),
    );
    let unavailable = registry.unavailable();
    assert_eq!(
        unavailable.len(),
        1,
        "only the one that failed: {unavailable:?}"
    );
    assert_eq!(unavailable[0].0, "broken");
    assert!(unavailable[0].1.contains("NEVER_SET"), "{unavailable:?}");
}

/// A disabled connection is not a broken one: the operator said so, and saying
/// it is what lets a host start without that provider. Reporting it here would
/// make disabling useless, since disabling is the remedy being offered.
#[test]
fn a_disabled_connection_is_not_reported_as_unable_to_authenticate() {
    let registry = registry(
        r#"
[connections.working]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }

[connections.off]
provider = "openrouter"
disabled = true
auth = { kind = "env", key = "NEVER_SET" }
"#,
        environment(&[("ANTHROPIC_KEY", "secret")]),
    );
    assert!(
        registry.unavailable().is_empty(),
        "{:?}",
        registry.unavailable()
    );
}

/// `auth = { kind = "oauth" }` reads the grant efference stores for itself,
/// rather than the file Codex CLI keeps (docs/transport.md).
#[test]
fn an_oauth_connection_loads_the_stored_grant() {
    let secrets = Arc::new(Secrets::in_memory());
    let access = crate::inference::chatgpt::test_support::access_token("acct-7", "plus");
    crate::inference::chatgpt::test_support::store_grant(&secrets, "codex", &access);

    let registry = registry_with_secrets(
        r#"
[connections.codex]
provider = "openai-codex"
auth = { kind = "oauth" }
"#,
        environment(&[]),
        &secrets,
    );
    assert!(
        registry.unavailable().is_empty(),
        "{:?}",
        registry.unavailable()
    );
}

/// With no grant stored, the connection is unavailable and says how to fix it
/// — not a panic, and not a silent half-working host.
#[test]
fn an_oauth_connection_without_a_grant_says_to_log_in() {
    let registry = registry_with_secrets(
        r#"
[connections.codex]
provider = "openai-codex"
auth = { kind = "oauth" }
"#,
        environment(&[]),
        &Arc::new(Secrets::in_memory()),
    );
    let unavailable = registry.unavailable();
    assert_eq!(unavailable.len(), 1);
    assert!(
        unavailable[0].1.contains("efference setup"),
        "{unavailable:?}"
    );
}

/// The kind names a mechanism; which issuer it means is the provider's to say,
/// and only the Codex backend has one today.
#[test]
fn oauth_on_a_provider_with_no_issuer_is_refused() {
    let registry = registry_with_secrets(
        r#"
[connections.other]
provider = "anthropic"
auth = { kind = "oauth" }
"#,
        environment(&[]),
        &Arc::new(Secrets::in_memory()),
    );
    let unavailable = registry.unavailable();
    assert_eq!(unavailable.len(), 1);
    assert!(unavailable[0].1.contains("no issuer"), "{unavailable:?}");
}
