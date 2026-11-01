use super::*;

use crate::platform::www_authenticate::Challenges;

fn protected(json: serde_json::Value) -> ProtectedResource {
    serde_json::from_value(json).unwrap()
}

fn challenge(header: &str) -> Challenges {
    Challenges::parse(header)
}

/// The document this renders is served at [`CLIENT_ID_METADATA_DOCUMENT`], and
/// an authorization server fetches it to decide whether to honor an
/// authorization request. If the two disagree — most importantly on
/// `redirect_uris` — every authorization against a CIMD-supporting server
/// fails with an opaque `invalid_request`.
///
/// So this pins what must be hosted. A failure here is not a bug in the test:
/// it means the hosted document needs updating to match.
#[test]
fn the_rendered_document_matches_what_is_hosted() {
    let rendered: serde_json::Value = serde_json::from_str(
        &client_metadata(None)
            .as_metadata_document(CLIENT_ID_METADATA_DOCUMENT)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        rendered,
        serde_json::json!({
            "client_id": "https://efference.net/oauth/client-v1.json",
            "client_name": "efference",
            "redirect_uris": [
                "http://127.0.0.1:50505/callback", "http://localhost:50505/callback",
                "http://127.0.0.1:50506/callback", "http://localhost:50506/callback",
                "http://127.0.0.1:50507/callback", "http://localhost:50507/callback"
            ],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        })
    );
}

/// The `client_id` URL is an identity, not a location to be normalized: it
/// must be https, must have a path component, and must not redirect.
#[test]
fn the_client_id_url_meets_the_cimd_requirements() {
    let url = Url::parse(CLIENT_ID_METADATA_DOCUMENT).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_ne!(
        url.path(),
        "/",
        "a client_id URL must have a path component"
    );
}

#[test]
fn client_metadata_declares_every_rung_in_both_spellings() {
    let metadata = client_metadata(None);
    assert_eq!(metadata.redirect_uris.len(), REDIRECT_PORTS.len() * 2);
    for port in REDIRECT_PORTS {
        assert!(
            metadata
                .redirect_uris
                .contains(&format!("http://127.0.0.1:{port}/callback"))
        );
        assert!(
            metadata
                .redirect_uris
                .contains(&format!("http://localhost:{port}/callback"))
        );
    }
}

// --- scope selection ---

/// The challenge's scope is authoritative for the operation that failed, so it
/// wins over the resource's advertised set.
#[test]
fn a_challenge_scope_wins_over_scopes_supported() {
    let challenges = challenge(r#"Bearer scope="files:write""#);
    assert_eq!(
        select_scope(
            challenges.scheme("Bearer"),
            &protected(serde_json::json!({
                "resource": "https://mcp.example.com",
                "scopes_supported": ["files:read"],
            }))
        ),
        Some("files:write".into())
    );
}

#[test]
fn scopes_supported_is_the_fallback() {
    let challenges = challenge("Bearer");
    assert_eq!(
        select_scope(
            challenges.scheme("Bearer"),
            &protected(serde_json::json!({
                "resource": "https://mcp.example.com",
                "scopes_supported": ["files:read", "files:write"],
            }))
        ),
        Some("files:read files:write".into())
    );
}

/// With neither, the `scope` parameter is omitted rather than sent empty.
#[test]
fn no_scope_anywhere_yields_none() {
    let bare = protected(serde_json::json!({"resource": "https://mcp.example.com"}));
    assert_eq!(select_scope(None, &bare), None);

    let empty = protected(serde_json::json!({
        "resource": "https://mcp.example.com",
        "scopes_supported": [],
    }));
    assert_eq!(select_scope(None, &empty), None);
}

// --- resource identity ---

/// A resource may identify itself more coarsely than the path it is reached
/// at; that is normal and explicitly contemplated by the spec.
#[test]
fn a_coarser_resource_identifier_on_the_same_origin_is_accepted() {
    let url = Url::parse("https://mcp.example.com/mcp").unwrap();
    for resource in [
        "https://mcp.example.com",
        "https://mcp.example.com/",
        "https://mcp.example.com/mcp",
    ] {
        assert!(
            check_resource(&url, &protected(serde_json::json!({"resource": resource}))).is_ok(),
            "{resource} was rejected"
        );
    }
}

/// A different origin means the document describes something else, and a token
/// minted for it would carry the wrong audience.
#[test]
fn a_different_origin_is_refused() {
    let url = Url::parse("https://mcp.example.com/mcp").unwrap();
    for resource in [
        "https://attacker.example",
        "https://mcp.example.com:8443",
        "http://mcp.example.com",
        "https://sub.mcp.example.com",
    ] {
        let error = check_resource(&url, &protected(serde_json::json!({"resource": resource})))
            .unwrap_err()
            .to_string();
        assert!(error.contains("different origin"), "{resource}: {error}");
    }
}

#[test]
fn a_resource_that_is_not_a_url_is_refused() {
    let url = Url::parse("https://mcp.example.com/mcp").unwrap();
    let error = check_resource(
        &url,
        &protected(serde_json::json!({"resource": "mcp.example.com"})),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("not a URL"), "{error}");
}

// --- client identity ---

/// A metadata-document identity is self-hosted and resolved on demand, so
/// there is nothing to store; a dynamic registration is bound to one issuer
/// and is exactly what needs storing.
#[test]
fn only_a_dynamic_registration_is_worth_persisting() {
    assert_eq!(
        ClientId::MetadataDocument.client_id(),
        CLIENT_ID_METADATA_DOCUMENT
    );
    assert!(ClientId::MetadataDocument.into_persistable().is_none());

    let registered = ClientId::Registered(Registration {
        client_id: "abc".into(),
        client_secret: None,
        issuer: "https://auth.example.com".into(),
    });
    assert_eq!(registered.client_id(), "abc");
    let persisted = registered.into_persistable().unwrap();
    assert_eq!(persisted.issuer, "https://auth.example.com");
}
