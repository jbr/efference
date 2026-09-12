use super::*;

fn tokens(json: serde_json::Value) -> Tokens {
    serde_json::from_value(json).unwrap()
}

/// A grant as it looks coming out of a token response, round-tripped through
/// the store's own serialization.
fn stored() -> StoredAuth {
    StoredAuth {
        issuer: "https://mcp.sentry.dev".into(),
        resource: "https://mcp.sentry.dev/mcp".into(),
        token_endpoint: "https://mcp.sentry.dev/oauth/token".into(),
        client_id: "https://efference.net/oauth/client-v1.json".into(),
        tokens: tokens(serde_json::json!({
            "access_token": "at-1",
            "token_type": "Bearer",
            "refresh_token": "rt-1",
            "scope": "org:read",
            "expires_at": 1_800_000_000u64,
        })),
        registration: None,
    }
}

fn store() -> (KeychainStore, Arc<Secrets>) {
    let secrets = Arc::new(Secrets::in_memory());
    (KeychainStore::new(Arc::clone(&secrets)), secrets)
}

/// The normal first-run state: no item yet is not an error.
#[test]
fn loading_an_absent_server_yields_nothing() {
    let (store, _secrets) = store();
    assert!(store.load("sentry").unwrap().is_none());
}

/// The item is filed under the population prefix, so an inference key named
/// like a server can never be read as a grant.
#[test]
fn a_grant_is_filed_under_the_mcp_prefix() {
    let (store, secrets) = store();
    store.save("sentry", &stored()).unwrap();
    assert!(secrets.get("mcp:sentry").unwrap().is_some());
    assert!(secrets.get("sentry").unwrap().is_none());
}

#[test]
fn a_grant_round_trips() {
    let (store, _secrets) = store();
    store.save("sentry", &stored()).unwrap();

    let loaded = store.load("sentry").unwrap().unwrap();
    assert_eq!(loaded.issuer, "https://mcp.sentry.dev");
    assert_eq!(loaded.resource, "https://mcp.sentry.dev/mcp");
    assert_eq!(loaded.tokens.access_token(), "at-1");
    assert_eq!(loaded.tokens.refresh_token(), Some("rt-1"));
    assert_eq!(loaded.tokens.scope(), Some("org:read"));
    assert_eq!(loaded.tokens.authorization_header(), "Bearer at-1");
}

/// Expiry is stored as an absolute instant, so a grant saved near its deadline
/// is still expired when it comes back — a duration would silently reset.
#[test]
fn an_expiry_survives_the_round_trip_as_an_instant() {
    let (store, _secrets) = store();
    let mut auth = stored();
    auth.tokens = tokens(serde_json::json!({
        "access_token": "at",
        "token_type": "Bearer",
        "expires_at": 100u64,
    }));
    store.save("old", &auth).unwrap();
    assert!(store.load("old").unwrap().unwrap().tokens.is_expired());
}

/// A `client_id` is only valid at the server that issued it, so the binding has
/// to survive storage — dropping it would let a stored registration be replayed
/// at a different authorization server.
#[test]
fn a_registration_keeps_its_issuer_through_storage() {
    let (store, _secrets) = store();
    let mut auth = stored();
    auth.registration = Some(Registration {
        client_id: "dcr-abc".into(),
        client_secret: None,
        issuer: "https://mcp.openrouter.ai".into(),
    });
    store.save("openrouter", &auth).unwrap();

    let registration = store
        .load("openrouter")
        .unwrap()
        .unwrap()
        .registration
        .unwrap();
    assert_eq!(registration.issuer, "https://mcp.openrouter.ai");
    assert!(registration.is_for("https://mcp.openrouter.ai"));
    assert!(!registration.is_for("https://mcp.sentry.dev"));
}

#[test]
fn several_servers_coexist_and_are_independent() {
    let (store, _secrets) = store();
    let mut other = stored();
    other.issuer = "https://mcp.openrouter.ai".into();
    store.save("sentry", &stored()).unwrap();
    store.save("openrouter", &other).unwrap();

    assert_eq!(
        store.load("sentry").unwrap().unwrap().issuer,
        "https://mcp.sentry.dev"
    );
    assert_eq!(
        store.load("openrouter").unwrap().unwrap().issuer,
        "https://mcp.openrouter.ai"
    );

    // Clearing one leaves the other alone — `--reauth sentry` must not log the
    // operator out of everything.
    store.clear("sentry").unwrap();
    assert!(store.load("sentry").unwrap().is_none());
    assert!(store.load("openrouter").unwrap().is_some());
}

#[test]
fn saving_twice_replaces_rather_than_duplicates() {
    let (store, _secrets) = store();
    store.save("sentry", &stored()).unwrap();
    let mut rotated = stored();
    rotated.tokens = tokens(serde_json::json!({
        "access_token": "at-2",
        "token_type": "Bearer",
        "refresh_token": "rt-2",
        "expires_at": null,
    }));
    store.save("sentry", &rotated).unwrap();

    let loaded = store.load("sentry").unwrap().unwrap();
    assert_eq!(loaded.tokens.access_token(), "at-2");
    assert_eq!(loaded.tokens.refresh_token(), Some("rt-2"));
}

#[test]
fn clearing_something_absent_is_not_an_error() {
    let (store, _secrets) = store();
    assert!(store.clear("never-existed").is_ok());
    store.save("sentry", &stored()).unwrap();
    assert!(store.clear("never-existed").is_ok());
    assert!(store.load("sentry").unwrap().is_some());
}

/// A corrupt item is a hard error, not a silent "you have no credentials" —
/// the latter would quietly restart the browser dance and overwrite whatever
/// was actually in there.
#[test]
fn a_corrupt_item_is_an_error_rather_than_an_empty_store() {
    let (store, secrets) = store();
    secrets.set("mcp:sentry", "{not json").unwrap();
    let error = store.load("sentry").unwrap_err().to_string();
    assert!(error.contains("stored credentials"), "{error}");
}
