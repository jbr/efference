use super::*;

const ISSUER: &str = "https://auth.example.com";

fn server(json: serde_json::Value) -> AuthorizationServer {
    serde_json::from_value(json).unwrap()
}

fn plain_server() -> AuthorizationServer {
    server(serde_json::json!({
        "issuer": ISSUER,
        "authorization_endpoint": "https://auth.example.com/authorize",
        "token_endpoint": "https://auth.example.com/token",
        "code_challenge_methods_supported": ["S256"],
    }))
}

fn iss_advertising_server() -> AuthorizationServer {
    server(serde_json::json!({
        "issuer": ISSUER,
        "authorization_endpoint": "https://auth.example.com/authorize",
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
    }))
}

fn start(server: &AuthorizationServer) -> (Pending, Url) {
    Pending::new(
        server,
        "client-abc",
        "http://127.0.0.1:50505/callback",
        "https://mcp.example.com/mcp",
        Some("files:read".into()),
    )
    .unwrap()
}

/// Decoded query pairs of a callback URL, the shape `validate` consumes.
fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn query(url: &Url) -> Vec<(String, String)> {
    url.query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[test]
fn the_authorization_url_carries_every_required_parameter() {
    let (pending, url) = start(&plain_server());
    let query = query(&url);
    let get = |name: &str| {
        query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert_eq!(url.path(), "/authorize");
    assert_eq!(get("response_type"), "code");
    assert_eq!(get("client_id"), "client-abc");
    assert_eq!(get("redirect_uri"), "http://127.0.0.1:50505/callback");
    assert_eq!(get("code_challenge_method"), "S256");
    assert_eq!(get("resource"), "https://mcp.example.com/mcp");
    assert_eq!(get("scope"), "files:read");
    assert_eq!(get("state"), pending.state());
    // The challenge travels; the verifier does not.
    assert!(!url.as_str().contains(pending.pkce.verifier()));
    assert!(url.as_str().contains(pending.pkce.challenge()));
}

#[test]
fn the_scope_parameter_is_omitted_when_there_is_none() {
    let (_, url) = Pending::new(
        &plain_server(),
        "client-abc",
        "http://127.0.0.1:50505/callback",
        "https://mcp.example.com",
        None,
    )
    .unwrap();
    assert!(!query(&url).iter().any(|(k, _)| k == "scope"));
}

/// RFC 8414 permits an authorization endpoint to carry its own query
/// parameters; replacing the query rather than appending would break it.
#[test]
fn existing_endpoint_query_parameters_are_preserved() {
    let server = server(serde_json::json!({
        "issuer": ISSUER,
        "authorization_endpoint": "https://auth.example.com/authorize?tenant=acme",
        "code_challenge_methods_supported": ["S256"],
    }));
    let (_, url) = start(&server);
    let query = query(&url);
    assert!(
        query.contains(&("tenant".into(), "acme".into())),
        "{query:?}"
    );
    assert!(query.iter().any(|(k, _)| k == "code_challenge"));
}

#[test]
fn each_request_gets_a_fresh_state_and_verifier() {
    let (first, _) = start(&plain_server());
    let (second, _) = start(&plain_server());
    assert_ne!(first.state(), second.state());
    assert_ne!(first.pkce.verifier(), second.pkce.verifier());
}

#[test]
fn a_matching_response_yields_the_code_and_verifier() {
    let (pending, _) = start(&plain_server());
    let verifier = pending.pkce.verifier().to_string();
    let state = pending.state().to_string();
    let authorized = pending
        .validate(&params(&[("code", "the-code"), ("state", &state)]))
        .unwrap();
    assert_eq!(authorized.code, "the-code");
    assert_eq!(authorized.code_verifier, verifier);
    assert_eq!(authorized.redirect_uri, "http://127.0.0.1:50505/callback");
}

// --- The RFC 9207 table ---

#[test]
fn iss_advertised_and_matching_proceeds() {
    let (pending, _) = start(&iss_advertising_server());
    let state = pending.state().to_string();
    assert!(
        pending
            .validate(&params(&[
                ("code", "c"),
                ("state", &state),
                ("iss", ISSUER)
            ]))
            .is_ok()
    );
}

#[test]
fn iss_advertised_but_absent_is_rejected() {
    let (pending, _) = start(&iss_advertising_server());
    let state = pending.state().to_string();
    let error = pending
        .validate(&params(&[("code", "c"), ("state", &state)]))
        .unwrap_err()
        .to_string();
    assert!(error.contains("no iss"), "{error}");
}

/// The local-policy row: a present `iss` is compared regardless of what the
/// metadata advertised, so a server emitting `iss` before updating its
/// metadata is still checked.
#[test]
fn iss_unadvertised_but_present_is_still_compared() {
    let (pending, _) = start(&plain_server());
    let state = pending.state().to_string();
    let error = pending
        .validate(&params(&[
            ("code", "c"),
            ("state", &state),
            ("iss", "https://attacker.example"),
        ]))
        .unwrap_err()
        .to_string();
    assert!(error.contains("attacker.example"), "{error}");
}

#[test]
fn iss_unadvertised_and_absent_proceeds() {
    let (pending, _) = start(&plain_server());
    let state = pending.state().to_string();
    assert!(
        pending
            .validate(&params(&[("code", "c"), ("state", &state)]))
            .is_ok()
    );
}

/// Normalizing before comparison would make two distinct issuer identifiers
/// compare equal, which is the attack the check exists to stop.
#[test]
fn iss_comparison_applies_no_normalization() {
    for near_miss in [
        "https://auth.example.com/",
        "https://AUTH.example.com",
        "https://auth.example.com:443",
        "HTTPS://auth.example.com",
    ] {
        let (pending, _) = start(&plain_server());
        let state = pending.state().to_string();
        assert!(
            pending
                .validate(&params(&[
                    ("code", "c"),
                    ("state", &state),
                    ("iss", near_miss)
                ]))
                .is_err(),
            "{near_miss} was accepted as {ISSUER}"
        );
    }
}

// --- state, errors, and ordering ---

#[test]
fn a_wrong_or_missing_state_is_rejected() {
    for response in [
        params(&[("code", "c"), ("state", "someone-elses-state")]),
        params(&[("code", "c")]),
    ] {
        let (pending, _) = start(&plain_server());
        let error = pending.validate(&response).unwrap_err().to_string();
        assert!(error.contains("state"), "{error}");
    }
}

#[test]
fn an_error_response_is_surfaced_with_its_description() {
    let (pending, _) = start(&plain_server());
    let state = pending.state().to_string();
    let error = pending
        .validate(&params(&[
            ("error", "access_denied"),
            ("error_description", "the user said no"),
            ("state", &state),
        ]))
        .unwrap_err()
        .to_string();
    assert!(error.contains("access_denied"), "{error}");
    assert!(error.contains("the user said no"), "{error}");
}

/// The spec is explicit that on an `iss` mismatch the client must not act on
/// or display `error`, `error_description`, or `error_uri` — so the issuer
/// check has to run before anything reads them.
#[test]
fn a_mismatched_error_response_does_not_leak_its_message() {
    let (pending, _) = start(&plain_server());
    let state = pending.state().to_string();
    let error = pending
        .validate(&params(&[
            ("error", "access_denied"),
            ("error_description", "ATTACKER CONTROLLED TEXT"),
            ("state", &state),
            ("iss", "https://attacker.example"),
        ]))
        .unwrap_err()
        .to_string();
    assert!(!error.contains("ATTACKER CONTROLLED TEXT"), "{error}");
    assert!(!error.contains("access_denied"), "{error}");
    assert!(error.contains("attacker.example"), "{error}");
}

#[test]
fn a_response_with_neither_code_nor_error_is_rejected() {
    let (pending, _) = start(&plain_server());
    let state = pending.state().to_string();
    let error = pending
        .validate(&params(&[("state", &state)]))
        .unwrap_err()
        .to_string();
    assert!(error.contains("no code"), "{error}");

    let (pending, _) = start(&plain_server());
    let state = pending.state().to_string();
    assert!(
        pending
            .validate(&params(&[("state", &state), ("code", "")]))
            .is_err()
    );
}

// --- scope accumulation ---

/// Re-authorizing with only the challenged scopes would drop permissions
/// already granted for other operations.
#[test]
fn a_step_up_unions_previous_and_challenged_scopes() {
    assert_eq!(
        union_scopes(Some("files:read"), Some("files:write")),
        Some("files:read files:write".into())
    );
}

#[test]
fn the_union_deduplicates_and_is_order_stable() {
    assert_eq!(
        union_scopes(Some("b a"), Some("a c")),
        union_scopes(Some("c a"), Some("a b"))
    );
    assert_eq!(union_scopes(Some("a b"), Some("b a")), Some("a b".into()));
}

#[test]
fn the_union_of_nothing_is_nothing() {
    assert_eq!(union_scopes(None, None), None);
    assert_eq!(union_scopes(Some("  "), None), None);
    assert_eq!(
        union_scopes(None, Some("files:read")),
        Some("files:read".into())
    );
}
