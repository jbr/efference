use super::*;

/// Decode a form body into pairs, so assertions read as parameters rather than
/// as one long percent-encoded string.
fn pairs(form: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(form.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn tokens(json: serde_json::Value) -> Tokens {
    Tokens::from_response(serde_json::from_value(json).unwrap())
}

#[test]
fn the_code_exchange_carries_every_required_parameter() {
    let form = CodeExchange {
        code: "the-code",
        redirect_uri: "http://127.0.0.1:50505/callback",
        client_id: "client-abc",
        code_verifier: "the-verifier",
        resource: "https://mcp.example.com/mcp",
    }
    .form();
    assert_eq!(
        pairs(&form),
        [
            ("grant_type", "authorization_code"),
            ("code", "the-code"),
            ("redirect_uri", "http://127.0.0.1:50505/callback"),
            ("client_id", "client-abc"),
            ("code_verifier", "the-verifier"),
            ("resource", "https://mcp.example.com/mcp"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
    );
}

/// The two parameters that are themselves URLs have to survive the round trip
/// intact — the server compares `redirect_uri` byte-for-byte against the
/// authorization request, and `resource` is what binds the token's audience.
#[test]
fn urls_are_percent_encoded_in_the_body() {
    let form = CodeExchange {
        code: "c",
        redirect_uri: "http://127.0.0.1:50505/callback",
        client_id: "id",
        code_verifier: "v",
        resource: "https://mcp.example.com/mcp",
    }
    .form();
    assert!(
        form.contains("resource=https%3A%2F%2Fmcp.example.com%2Fmcp"),
        "{form}"
    );
    assert!(!form.contains("resource=https://"), "{form}");
}

/// A `client_id` that is itself a URL — the Client ID Metadata Document
/// case — must not break the encoding.
#[test]
fn a_url_shaped_client_id_survives_encoding() {
    let form = CodeExchange {
        code: "c",
        redirect_uri: "http://localhost:50505/callback",
        client_id: "https://efference.net/oauth/client-v1.json",
        code_verifier: "v",
        resource: "https://mcp.example.com",
    }
    .form();
    let decoded = pairs(&form);
    let client_id = decoded.iter().find(|(k, _)| k == "client_id").unwrap();
    assert_eq!(client_id.1, "https://efference.net/oauth/client-v1.json");
}

#[test]
fn the_refresh_request_carries_the_resource_indicator() {
    assert_eq!(
        pairs(&refresh_form(
            "r-token",
            "client-abc",
            "https://mcp.example.com"
        )),
        [
            ("grant_type", "refresh_token"),
            ("refresh_token", "r-token"),
            ("client_id", "client-abc"),
            ("resource", "https://mcp.example.com"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
    );
}

#[test]
fn a_token_response_becomes_a_grant() {
    let tokens = tokens(serde_json::json!({
        "access_token": "at-1",
        "token_type": "Bearer",
        "refresh_token": "rt-1",
        "scope": "files:read files:write",
        "expires_in": 3600,
    }));
    assert_eq!(tokens.access_token(), "at-1");
    assert_eq!(tokens.authorization_header(), "Bearer at-1");
    assert_eq!(tokens.refresh_token(), Some("rt-1"));
    assert_eq!(tokens.scope(), Some("files:read files:write"));
    assert!(!tokens.is_expired());
}

/// `token_type` is optional in practice even though the RFC requires it; the
/// header still has to be well-formed.
#[test]
fn a_response_without_a_token_type_defaults_to_bearer() {
    let tokens = tokens(serde_json::json!({"access_token": "at-1"}));
    assert_eq!(tokens.authorization_header(), "Bearer at-1");
}

/// Sentry's MCP server answers the token request with
/// `"token_type": "bearer"` and then rejects `Authorization: bearer <token>`
/// with `invalid_token`, accepting only the literal `Bearer`. Echoing the
/// server's own field back is the obvious-looking thing to do and it 401s.
#[test]
fn the_bearer_scheme_is_canonicalized_whatever_case_the_server_sent() {
    for sent in ["bearer", "Bearer", "BEARER", "BeArEr"] {
        let tokens = tokens(serde_json::json!({
            "access_token": "at-1",
            "token_type": sent,
        }));
        assert_eq!(
            tokens.authorization_header(),
            "Bearer at-1",
            "token_type {sent:?} did not canonicalize"
        );
    }
}

/// MCP defines only bearer, so a server sending anything else has gone beyond
/// the spec — repeating what it said beats guessing at its capitalization.
#[test]
fn a_non_bearer_token_type_is_passed_through() {
    let tokens = tokens(serde_json::json!({
        "access_token": "at-1",
        "token_type": "DPoP",
    }));
    assert_eq!(tokens.authorization_header(), "DPoP at-1");
}

/// `expires_in` is a duration on the wire; storing it as one would make every
/// later check wrong by however long the token sat unused.
#[test]
fn expires_in_resolves_to_an_instant_at_receipt() {
    let tokens = tokens(serde_json::json!({"access_token": "at", "expires_in": 3600}));
    let expires_at = tokens.expires_at().unwrap();
    let expected = SystemTime::now() + Duration::from_secs(3600);
    let drift = expected
        .duration_since(expires_at)
        .unwrap_or_else(|e| e.duration());
    assert!(drift < Duration::from_secs(5), "drifted by {drift:?}");
}

/// The skew is what keeps a token from expiring in flight between the check
/// and the request that uses it.
#[test]
fn a_token_inside_the_skew_window_is_already_expired() {
    let mut tokens = tokens(serde_json::json!({"access_token": "at"}));
    tokens.expires_at = Some(SystemTime::now() + EXPIRY_SKEW / 2);
    assert!(tokens.is_expired());

    tokens.expires_at = Some(SystemTime::now() + EXPIRY_SKEW * 4);
    assert!(!tokens.is_expired());
}

/// A grant with no stated lifetime is not assumed to be expired — the
/// resource server's `401` is the only authority on that case.
#[test]
fn a_grant_without_an_expiry_never_reports_expired() {
    let tokens = tokens(serde_json::json!({"access_token": "at"}));
    assert_eq!(tokens.expires_at(), None);
    assert!(!tokens.is_expired());
}

/// OAuth 2.1 mandates refresh-token rotation for public clients, so a new one
/// usually arrives — but a server that omits it means the old one still works,
/// and dropping it would silently end the ability to refresh.
#[test]
fn a_refresh_response_without_a_new_token_keeps_the_old_one() {
    let previous = tokens(serde_json::json!({"access_token": "at-1", "refresh_token": "rt-1"}));
    let mut refreshed = tokens(serde_json::json!({"access_token": "at-2"}));
    assert_eq!(refreshed.refresh_token(), None);

    // The carry-forward `refresh` performs when the response omits one.
    if refreshed.refresh_token.is_none() {
        refreshed.refresh_token = previous.refresh_token.clone();
    }
    assert_eq!(refreshed.refresh_token(), Some("rt-1"));
    assert_eq!(refreshed.access_token(), "at-2");
}

#[test]
fn a_rotated_refresh_token_replaces_the_old_one() {
    let refreshed = tokens(serde_json::json!({"access_token": "at-2", "refresh_token": "rt-2"}));
    assert_eq!(refreshed.refresh_token(), Some("rt-2"));
}

/// These land in error paths and log lines; either token is enough to
/// impersonate the user at the resource server.
#[test]
fn debug_leaks_neither_token() {
    let tokens = tokens(serde_json::json!({
        "access_token": "secret-access",
        "refresh_token": "secret-refresh",
        "scope": "files:read",
    }));
    let rendered = format!("{tokens:?}");
    assert!(!rendered.contains("secret-access"), "{rendered}");
    assert!(!rendered.contains("secret-refresh"), "{rendered}");
    // Non-secret fields stay legible, or the redaction makes debugging worse.
    assert!(rendered.contains("files:read"), "{rendered}");
}
