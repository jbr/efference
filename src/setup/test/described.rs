//! Adding an endpoint this build knows no name for: what it asks, what it
//! suggests, and the two keys it writes (docs/transport.md).

use super::*;

/// The whole path, with the key already in the shell. The endpoint's host
/// supplies the connection name and the environment variable's name, so the two
/// questions that would otherwise have no sensible default arrive prefilled.
#[tokio::test]
async fn a_described_endpoint_is_named_after_its_host() {
    let probe = Table {
        env: BTreeMap::from([("META_API_KEY".to_string(), "sk-real".to_string())]),
        accepted_keys: strings(&["sk-real"]),
        known_models: strings(&["muse-spark-1.3", "muse-spark-1.3-contributor"]),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("Something else"),
            Answer::Pick("Anthropic Messages"),
            Answer::Type("https://api.meta.ai/v1"),
            // The suggested connection name, taken from the host.
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            // The suggested variable name, taken from the connection.
            Answer::Type(""),
            Answer::No,
            // The model question lists the models the endpoint serves.
            Answer::Pick("muse-spark-1.3"),
        ],
        &probe,
        "",
    )
    .await;
    let draft = draft.unwrap();
    assert!(script.said("META_API_KEY is set in this shell"));
    assert!(script.said("accepted the key (test key)"));
    let (name, connection) = &draft.connections[0];
    assert_eq!(name, "meta");
    assert_eq!(
        connection.provider,
        crate::api::Provider::Custom(WireDialect::Messages)
    );
    assert_eq!(
        connection.base_url.as_deref(),
        Some("https://api.meta.ai/v1")
    );

    // Written as what it is — a dialect and a base URL — never as a `provider`
    // line naming something that is not a provider name.
    let rendered = render("", &draft).unwrap();
    assert!(rendered.contains("[connections.meta]"), "{rendered}");
    assert!(rendered.contains("dialect = \"messages\""), "{rendered}");
    assert!(
        rendered.contains("base-url = \"https://api.meta.ai/v1\""),
        "{rendered}"
    );
    assert!(!rendered.contains("provider = \"custom"), "{rendered}");

    // And the host reads back what setup wrote, which is the only test that
    // matters for the file: the two halves resolve to the same connection.
    let config = crate::inference::InferenceConfig::from_toml(&rendered).unwrap();
    let registry = crate::inference::InferenceRegistry::from_config(
        config,
        std::path::Path::new("/host"),
        crate::env_table::EnvSnapshot::new(BTreeMap::from([(
            "META_API_KEY".to_string(),
            "sk-real".to_string(),
        )]))
        .unwrap(),
        &std::sync::Arc::new(crate::secrets::Secrets::in_memory()),
    )
    .unwrap();
    let inventory = registry.inventory();
    assert_eq!(inventory[0].name, "meta");
    assert_eq!(
        inventory[0].provider,
        crate::api::Provider::Custom(WireDialect::Messages)
    );
    assert!(inventory[0].available);
}

/// A base URL that is not one is refused at the prompt, with the shape spelled
/// out — the host would refuse it at load anyway, and later is worse.
#[tokio::test]
async fn a_base_url_that_is_not_a_url_is_refused_at_the_prompt() {
    let probe = Table {
        env: BTreeMap::from([("EXAMPLE_API_KEY".to_string(), "sk-real".to_string())]),
        accepted_keys: strings(&["sk-real"]),
        known_models: strings(&["m"]),
        ..Default::default()
    };
    let (draft, script) = run_script(
        vec![
            Answer::Pick("Something else"),
            Answer::Pick("OpenAI Responses"),
            Answer::Type("api.example.com/v1"),
            Answer::Type("https://api.example.com/v1"),
            Answer::Type(""),
            Answer::Pick("An environment variable"),
            Answer::Type(""),
            Answer::No,
            Answer::Pick("m"),
        ],
        &probe,
        "",
    )
    .await;
    assert!(script.said("A base URL starts with https://"));
    let (name, connection) = &draft.unwrap().connections[0];
    assert_eq!(name, "example");
    assert_eq!(
        connection.provider,
        crate::api::Provider::Custom(WireDialect::Responses)
    );
}
