use super::*;

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn urls(list: &[Url]) -> Vec<String> {
    list.iter().map(Url::to_string).collect()
}

fn authorization_server(json: serde_json::Value) -> AuthorizationServer {
    serde_json::from_value(json).unwrap()
}

/// RFC 9728 §5.1: the challenge's `resource_metadata` is authoritative, so it
/// is tried first — but the probes stay as fallbacks, since a server may
/// advertise a URL that no longer answers.
#[test]
fn an_advertised_resource_metadata_url_is_tried_first() {
    assert_eq!(
        urls(&protected_resource_urls(
            &url("https://mcp.example.com/mcp"),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        )),
        [
            "https://mcp.example.com/.well-known/oauth-protected-resource",
            "https://mcp.example.com/.well-known/oauth-protected-resource/mcp",
            "https://mcp.example.com/.well-known/oauth-protected-resource",
        ]
    );
}

/// The example from the spec's discovery document: path insertion before root.
#[test]
fn protected_resource_probes_path_then_root() {
    assert_eq!(
        urls(&protected_resource_urls(
            &url("https://example.com/public/mcp"),
            None
        )),
        [
            "https://example.com/.well-known/oauth-protected-resource/public/mcp",
            "https://example.com/.well-known/oauth-protected-resource",
        ]
    );
}

/// A server at the origin root has no path to insert, so there is one probe.
#[test]
fn protected_resource_at_the_root_probes_once() {
    assert_eq!(
        urls(&protected_resource_urls(&url("https://example.com"), None)),
        ["https://example.com/.well-known/oauth-protected-resource"]
    );
    assert_eq!(
        urls(&protected_resource_urls(&url("https://example.com/"), None)),
        ["https://example.com/.well-known/oauth-protected-resource"]
    );
}

#[test]
fn an_unparseable_advertised_url_is_skipped_not_fatal() {
    assert_eq!(
        urls(&protected_resource_urls(
            &url("https://example.com/mcp"),
            Some("not a url")
        )),
        [
            "https://example.com/.well-known/oauth-protected-resource/mcp",
            "https://example.com/.well-known/oauth-protected-resource",
        ]
    );
}

/// The three spellings from the spec, in the required priority order. RFC 8414
/// inserts the issuer path after the well-known segment; OIDC Discovery
/// appends the well-known segment after the path; deployed servers do both.
#[test]
fn an_issuer_with_a_path_gets_three_rungs_in_order() {
    assert_eq!(
        urls(&authorization_server_urls(&url(
            "https://auth.example.com/tenant1"
        ))),
        [
            "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
            "https://auth.example.com/.well-known/openid-configuration/tenant1",
            "https://auth.example.com/tenant1/.well-known/openid-configuration",
        ]
    );
}

#[test]
fn an_issuer_without_a_path_gets_two_rungs_in_order() {
    assert_eq!(
        urls(&authorization_server_urls(&url("https://auth.example.com"))),
        [
            "https://auth.example.com/.well-known/oauth-authorization-server",
            "https://auth.example.com/.well-known/openid-configuration",
        ]
    );
}

/// A trailing slash on the issuer must not produce a doubled slash in the
/// probe path, which would 404 on most servers.
#[test]
fn a_trailing_slash_does_not_double_up() {
    assert_eq!(
        urls(&authorization_server_urls(&url(
            "https://auth.example.com/tenant1/"
        )))[0],
        "https://auth.example.com/.well-known/oauth-authorization-server/tenant1"
    );
}

#[test]
fn a_nonstandard_port_is_preserved() {
    assert_eq!(
        urls(&protected_resource_urls(
            &url("https://mcp.example.com:8443/mcp"),
            None
        ))[0],
        "https://mcp.example.com:8443/.well-known/oauth-protected-resource/mcp"
    );
}

/// A well-known URI is a path on an origin; carrying the MCP endpoint's query
/// string into the probe would ask for something else entirely.
#[test]
fn query_and_fragment_are_dropped_from_probes() {
    assert_eq!(
        urls(&protected_resource_urls(
            &url("https://example.com/mcp?session=abc#here"),
            None
        )),
        [
            "https://example.com/.well-known/oauth-protected-resource/mcp",
            "https://example.com/.well-known/oauth-protected-resource",
        ]
    );
}

/// The spec's own example of the attack this prevents: a document served from
/// one origin claiming to be the issuer for another.
#[test]
fn an_issuer_mismatch_is_rejected() {
    // The check itself is inline in `authorization_server`; this pins the
    // comparison it performs — byte-identical, no normalization.
    let metadata = authorization_server(serde_json::json!({"issuer": "https://honest.example"}));
    assert_ne!(metadata.issuer, "https://attacker.example");
    // Trailing-slash and case differences are mismatches, not near-misses.
    assert_ne!(metadata.issuer, "https://honest.example/");
    assert_ne!(metadata.issuer, "https://Honest.example");
}

/// A public client with no PKCE has nothing protecting its authorization code,
/// so absence of the field is a refusal rather than a default.
#[test]
fn a_server_without_pkce_support_is_refused() {
    let metadata = authorization_server(serde_json::json!({"issuer": "https://auth.example.com"}));
    let error = metadata.require_pkce().unwrap_err().to_string();
    assert!(error.contains("does not support"), "{error}");

    let empty = authorization_server(
        serde_json::json!({"issuer": "https://a.example", "code_challenge_methods_supported": []}),
    );
    assert!(empty.require_pkce().is_err());
}

#[test]
fn a_server_offering_only_plain_pkce_is_refused() {
    let metadata = authorization_server(
        serde_json::json!({"issuer": "https://a.example", "code_challenge_methods_supported": ["plain"]}),
    );
    let error = metadata.require_pkce().unwrap_err().to_string();
    assert!(error.contains("weaker"), "{error}");
}

#[test]
fn a_server_offering_s256_passes() {
    let metadata = authorization_server(
        serde_json::json!({"issuer": "https://a.example", "code_challenge_methods_supported": ["plain", "S256"]}),
    );
    assert!(metadata.require_pkce().is_ok());
}

/// Unknown fields are ignored: both documents are extensible and real servers
/// send more than the spec names.
#[test]
fn unknown_fields_do_not_break_parsing() {
    let metadata = authorization_server(serde_json::json!({
        "issuer": "https://a.example",
        "code_challenge_methods_supported": ["S256"],
        "authorization_endpoint": "https://a.example/authorize",
        "token_endpoint": "https://a.example/token",
        "something_the_spec_never_named": {"nested": true},
    }));
    assert_eq!(
        metadata.authorization_endpoint().unwrap(),
        "https://a.example/authorize"
    );
    assert_eq!(
        metadata.token_endpoint().unwrap(),
        "https://a.example/token"
    );
}

/// A document that parses but omits an endpoint is a real deployment state.
#[test]
fn a_missing_endpoint_names_itself() {
    let metadata = authorization_server(serde_json::json!({"issuer": "https://a.example"}));
    let error = metadata.authorization_endpoint().unwrap_err().to_string();
    assert!(error.contains("authorization_endpoint"), "{error}");
    assert!(error.contains("https://a.example"), "{error}");
}

#[test]
fn registration_and_cimd_support_read_off_the_document() {
    let neither = authorization_server(serde_json::json!({"issuer": "https://a.example"}));
    assert!(!neither.supports_client_id_metadata_document());
    assert_eq!(neither.registration_endpoint, None);

    let both = authorization_server(serde_json::json!({
        "issuer": "https://a.example",
        "registration_endpoint": "https://a.example/register",
        "client_id_metadata_document_supported": true,
    }));
    assert!(both.supports_client_id_metadata_document());
    assert_eq!(
        both.registration_endpoint.as_deref(),
        Some("https://a.example/register")
    );
}

/// The resource identifier from the document is what the RFC 8707 `resource`
/// parameter must carry — not the URL we happened to connect to.
#[test]
fn protected_resource_parses_the_spec_shape() {
    let metadata: ProtectedResource = serde_json::from_value(serde_json::json!({
        "resource": "https://mcp.example.com",
        "authorization_servers": ["https://auth.example.com", "https://auth2.example.com"],
        "scopes_supported": ["files:read"],
    }))
    .unwrap();
    assert_eq!(metadata.resource, "https://mcp.example.com");
    assert_eq!(metadata.authorization_servers.len(), 2);
    assert_eq!(
        metadata.scopes_supported.as_deref(),
        Some(["files:read".to_string()].as_slice())
    );
}

/// `authorization_servers` is required by MCP but the client still has to cope
/// with a document that omits it — that is a diagnosable server bug, not a panic.
#[test]
fn a_protected_resource_without_authorization_servers_parses_empty() {
    let metadata: ProtectedResource =
        serde_json::from_value(serde_json::json!({"resource": "https://mcp.example.com"})).unwrap();
    assert!(metadata.authorization_servers.is_empty());
    assert_eq!(metadata.scopes_supported, None);
}

/// Why the rung *order* is load-bearing, from a real deployment.
///
/// GitHub's authorization server issuer is `https://github.com/login/oauth` — a
/// path-carrying issuer. Both rung 1 (RFC 8414 path insertion) and rung 3 (OIDC
/// path append) answer `200` with a matching `issuer`, so either would pass the
/// identity check. But only rung 1's document advertises
/// `code_challenge_methods_supported`; rung 3's omits it, which `require_pkce`
/// refuses. Probing in the wrong order — or implementing only the OIDC-append
/// form, the one most often hand-rolled — means refusing to authorize against
/// GitHub at all.
#[test]
fn the_rung_order_puts_rfc_8414_path_insertion_first() {
    let rungs = urls(&authorization_server_urls(&url(
        "https://github.com/login/oauth",
    )));
    assert_eq!(
        rungs[0],
        "https://github.com/.well-known/oauth-authorization-server/login/oauth"
    );
    assert_eq!(
        rungs[2],
        "https://github.com/login/oauth/.well-known/openid-configuration"
    );
}

/// An authorization server on a loopback port whose document names `token_endpoint`
/// (or a loopback one when `None`), so discovery runs end to end over plain `http`.
/// The listener runs until the test's runtime ends.
async fn serving_document(token_endpoint: Option<&'static str>) -> String {
    let base = std::sync::Arc::new(std::sync::OnceLock::<String>::new());
    let served = std::sync::Arc::clone(&base);
    let handle = trillium_tokio::config()
        .without_signals()
        .listeners()
        .bind_tcp(("127.0.0.1", 0))
        .unwrap()
        .spawn(move |conn: trillium::Conn| {
            let base = std::sync::Arc::clone(&served);
            async move {
                let base = base.get().cloned().unwrap_or_default();
                let document = serde_json::json!({
                    "issuer": base,
                    "authorization_endpoint": format!("{base}/authorize"),
                    "token_endpoint": token_endpoint.map_or_else(|| format!("{base}/token"), str::to_string),
                    "code_challenge_methods_supported": ["S256"],
                });
                conn.with_status(200)
                    .with_response_header("content-type", "application/json")
                    .with_body(document.to_string())
            }
        });
    let issuer = format!(
        "http://{}",
        handle
            .info()
            .await
            .tcp_socket_addr()
            .expect("a TCP listener")
    );
    base.set(issuer.clone()).unwrap();
    issuer
}

/// A cleartext issuer on a host other than this machine is refused before any
/// request is made, and the error names the URL and the rule.
#[tokio::test]
async fn an_http_issuer_off_loopback_is_refused_at_discovery() {
    let error = Discovery::new()
        .authorization_server("http://auth.example.com")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("http://auth.example.com"), "{error}");
    assert!(error.contains("https"), "{error}");
    assert!(error.contains("loopback"), "{error}");
}

/// A protected resource reached over cleartext off loopback is refused the same way,
/// as is an advertised metadata URL that downgrades.
#[tokio::test]
async fn an_http_protected_resource_off_loopback_is_refused() {
    let discovery = Discovery::new();
    let error = discovery
        .protected_resource(&url("http://mcp.example.com/mcp"), None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("http://mcp.example.com/mcp"), "{error}");

    let error = discovery
        .protected_resource(
            &url("https://mcp.example.com/mcp"),
            Some("http://elsewhere.example.com/.well-known/oauth-protected-resource"),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("http://elsewhere.example.com"), "{error}");
}

/// A loopback `http` issuer is the local-lab case: discovery proceeds, and the
/// loopback endpoints in its document are allowed.
#[tokio::test]
async fn a_loopback_http_issuer_and_token_endpoint_are_allowed() {
    let issuer = serving_document(None).await;
    let server = Discovery::new()
        .authorization_server(&issuer)
        .await
        .expect("a loopback http issuer proceeds");
    assert_eq!(server.token_endpoint().unwrap(), format!("{issuer}/token"));
}

/// A document that is otherwise valid but names a cleartext token endpoint off
/// loopback is refused at fetch time, naming the endpoint.
#[tokio::test]
async fn a_non_loopback_http_token_endpoint_is_refused() {
    let issuer = serving_document(Some("http://auth.example.com/token")).await;
    let error = Discovery::new()
        .authorization_server(&issuer)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("token_endpoint"), "{error}");
    assert!(error.contains("http://auth.example.com/token"), "{error}");
}

/// The rule itself, over each endpoint the document can name and each accepted
/// loopback spelling.
#[test]
fn transport_accepts_https_and_loopback_http_only() {
    for allowed in [
        "https://auth.example.com/token",
        "http://localhost:8080/token",
        "http://127.0.0.1:8080/token",
        "http://[::1]:8080/token",
    ] {
        require_transport(&url(allowed), "token_endpoint").expect(allowed);
    }
    for refused in [
        "http://auth.example.com/token",
        "http://localhost.example.com/token",
        "http://127.0.0.1.example.com/token",
        "ftp://localhost/token",
    ] {
        require_transport(&url(refused), "token_endpoint").expect_err(refused);
    }
    for field in [
        "authorization_endpoint",
        "token_endpoint",
        "registration_endpoint",
    ] {
        let metadata = authorization_server(serde_json::json!({
            "issuer": "https://auth.example.com",
            field: "http://auth.example.com/x",
        }));
        let error = metadata.require_transport().unwrap_err().to_string();
        assert!(error.contains(field), "{field}: {error}");
    }
}
