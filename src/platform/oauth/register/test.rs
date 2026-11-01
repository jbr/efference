use super::*;

fn metadata() -> ClientMetadata {
    ClientMetadata::native(
        "efference",
        vec![
            "http://127.0.0.1:50505/callback".into(),
            "http://localhost:50505/callback".into(),
        ],
    )
}

/// A public native client: no secret to authenticate with, and an explicit
/// `application_type` so an OIDC server does not default it to `web` and
/// reject the loopback redirects.
#[test]
fn native_metadata_declares_a_public_client() {
    let json = serde_json::to_value(metadata()).unwrap();
    assert_eq!(json["token_endpoint_auth_method"], "none");
    assert_eq!(json["application_type"], "native");
    assert_eq!(json["response_types"], serde_json::json!(["code"]));
    assert_eq!(
        json["grant_types"],
        serde_json::json!(["authorization_code", "refresh_token"])
    );
    // Absent rather than null, so a strict server does not see an empty scope.
    assert!(json.get("scope").is_none());
}

#[test]
fn a_scope_is_included_when_one_is_known() {
    let json = serde_json::to_value(metadata().with_scope(Some("files:read".into()))).unwrap();
    assert_eq!(json["scope"], "files:read");
}

/// The document's `client_id` must equal the URL it is served from exactly;
/// an authorization server that fetches it checks precisely this.
#[test]
fn a_metadata_document_names_its_own_url() {
    let url = "https://efference.net/oauth/client-v1.json";
    let document = metadata().as_metadata_document(url).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&document).unwrap();
    assert_eq!(parsed["client_id"], url);
    assert_eq!(parsed["client_name"], "efference");
    assert_eq!(
        parsed["redirect_uris"],
        serde_json::json!([
            "http://127.0.0.1:50505/callback",
            "http://localhost:50505/callback"
        ])
    );
}

/// `application_type` is a registration-request parameter and has no meaning
/// in a hosted document.
#[test]
fn a_metadata_document_drops_the_registration_only_field() {
    let document = metadata()
        .as_metadata_document("https://efference.net/oauth/client-v1.json")
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&document).unwrap();
    assert!(parsed.get("application_type").is_none(), "{document}");
    // The fields the document does require are still there.
    assert_eq!(parsed["token_endpoint_auth_method"], "none");
}

/// Rendering the document from the same struct the registration request uses
/// is what keeps the hosted redirect URIs from drifting away from the ones
/// this binary actually listens on.
#[test]
fn the_document_and_the_registration_request_agree_on_redirect_uris() {
    let metadata = metadata();
    let request = serde_json::to_value(&metadata).unwrap();
    let document: serde_json::Value = serde_json::from_str(
        &metadata
            .as_metadata_document("https://efference.net/oauth/client-v1.json")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(request["redirect_uris"], document["redirect_uris"]);
}

/// A client identifier belongs to the authorization server that issued it.
/// Sending it elsewhere is not a degraded request, it is a request under
/// someone else's identity.
#[test]
fn a_registration_is_bound_to_its_issuer() {
    let registration = Registration {
        client_id: "abc".into(),
        client_secret: None,
        issuer: "https://auth.example.com".into(),
    };
    assert!(registration.is_for("https://auth.example.com"));
    assert!(!registration.is_for("https://other.example.com"));
    // No normalization: a trailing slash is a different issuer identifier.
    assert!(!registration.is_for("https://auth.example.com/"));
}

#[test]
fn a_registration_response_parses_with_or_without_a_secret() {
    let public: Registration =
        serde_json::from_value(serde_json::json!({"client_id": "abc"})).unwrap();
    assert_eq!(public.client_id, "abc");
    assert_eq!(public.client_secret, None);
    // The issuer is not part of the RFC 7591 response; it is filled in after.
    assert_eq!(public.issuer, "");

    let with_secret: Registration = serde_json::from_value(serde_json::json!({
        "client_id": "abc",
        "client_secret": "shh",
        "client_id_issued_at": 1700000000,
    }))
    .unwrap();
    assert_eq!(with_secret.client_secret.as_deref(), Some("shh"));
}

/// OIDC servers reject registrations whose redirect URIs conflict with the
/// declared application type, and the description is the only thing that says
/// which constraint was violated.
#[test]
fn an_oauth_error_body_renders_as_a_sentence() {
    assert_eq!(
        describe_error(
            r#"{"error":"invalid_redirect_uri","error_description":"loopback URIs require application_type native"}"#
        ),
        "invalid_redirect_uri (loopback URIs require application_type native)"
    );
    assert_eq!(
        describe_error(r#"{"error":"invalid_client_metadata"}"#),
        "invalid_client_metadata"
    );
}

#[test]
fn a_non_oauth_error_body_is_passed_through() {
    assert_eq!(describe_error("<html>502</html>"), "<html>502</html>");
    assert_eq!(describe_error(""), "no response body");
    assert_eq!(describe_error("   "), "no response body");
}

/// A registration that asks for a grant type the server does not advertise can
/// be rejected outright, failing the whole dance to gain a grant that server
/// was never going to issue. OpenRouter's authorization server advertises
/// `["authorization_code"]` and nothing else, so this is not hypothetical.
#[test]
fn grant_types_are_narrowed_to_what_the_server_advertises() {
    let supported = ["authorization_code".to_string()];
    let narrowed = metadata().restricted_to(Some(&supported));
    assert_eq!(narrowed.grant_types, ["authorization_code"]);
}

/// Absence of the field is not a claim about what is unsupported, so the full
/// list stands.
#[test]
fn a_server_advertising_no_grant_types_gets_the_full_list() {
    assert_eq!(
        metadata().restricted_to(None).grant_types,
        ["authorization_code", "refresh_token"]
    );
}

#[test]
fn a_server_advertising_both_keeps_both() {
    let supported = [
        "authorization_code".to_string(),
        "refresh_token".to_string(),
    ];
    assert_eq!(
        metadata().restricted_to(Some(&supported)).grant_types,
        ["authorization_code", "refresh_token"]
    );
}
