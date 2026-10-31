//! `QUERY /sessions`: a proposed session resolved without minting anything or
//! establishing an environment (docs/control-api.md).

use super::*;

/// `QUERY /sessions` (docs/control-api.md): the host's preview of a proposed
/// session — the effective selections, where each came from, and the issues that
/// would stop creation — without minting anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_sessions_previews_a_proposed_session_without_creating_one() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let client = host_client(&host, "one").await.with_prefix("");
    let mut request = SessionOptionsRequest::new(roots.0.join("one"));
    request.options.connection = Some("test".to_string());
    request.options.model = Some("m".to_string());
    let preview = client
        .session_options(&request)
        .await
        .expect("preview a proposed session");
    // Asserted through a re-encode: `preview` is the typed `ResolvedOptions` the
    // client decoded, so matching the wire again proves both the host's encoding
    // and that the client's decode of it loses nothing.
    assert_eq!(
        serde_json::to_value(&preview).unwrap(),
        serde_json::json!({
            "effective": {
                "main": {
                    "value": {"connection": "test", "provider": "anthropic", "model": "m"},
                    "source": "request",
                },
                "spotter": {
                    "value": {
                        "connection": "test",
                        "provider": "anthropic",
                        "model": "claude-haiku-4-5",
                    },
                    "source": "main_provider",
                },
                "triage": {"value": {"mode": "shared"}, "source": "shared_main"},
                "effort": {"value": null, "source": "provider_default"},
                "temperature": {"value": null, "source": "provider_default"},
            },
            "issues": [],
        })
    );
    assert!(host.ids().is_empty());
    host.swansong().shut_down().await;
}

/// **One bad field does not hide the good ones.** A malformed temperature is its
/// own issue; every independently resolvable selection still comes back, because
/// the form has to stay usable while the operator is mid-edit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_sessions_reports_an_invalid_field_without_hiding_the_rest() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    let response = server
        .build(trillium::Method::Query, "/sessions")
        .with_json_body(&serde_json::json!({
            "cwd": roots.0.join("one"),
            "connection": "test",
            "model": "m",
            "temperature": "260",
            "effort": "high",
        }))
        .await;
    response.assert_status(200);
    let body = json(response.body());
    assert_eq!(
        body["issues"],
        serde_json::json!([{
            "field": "temperature",
            "code": "invalid_type",
            "message": "must be a whole number from 0 to 100",
        }])
    );
    assert_eq!(body["effective"]["main"]["value"]["model"], "m");
    assert_eq!(
        body["effective"]["effort"],
        serde_json::json!({"value": "high", "source": "request"})
    );
    // The field that failed is the only one with no source.
    assert_eq!(
        body["effective"]["temperature"],
        serde_json::json!({"value": null, "source": null})
    );
    assert!(host.ids().is_empty());
    host.swansong().shut_down().await;
}

/// A body that is not a JSON object has no fields to resolve independently, so it
/// is the one shape that answers `400` rather than a preview.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_sessions_refuses_a_non_object_body() {
    let host = Host::new(one_connection_registry(), baseline_env());
    let server = host_server(&host).await;
    server
        .build(trillium::Method::Query, "/sessions")
        .with_json_body(&serde_json::json!(["not", "an", "object"]))
        .await
        .assert_status(400);
    host.swansong().shut_down().await;
}

/// `QUERY /sessions` mints nothing and establishes no environment, so it stays a
/// pure preview on a network listener (docs/control-api.md). `POST /sessions`
/// there behaves as it does on the socket: the route is one, and what a request
/// omits about its environment is the host's business rather than the
/// listener's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_network_host_previews_and_creates_the_same_way() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    let (server, bearer, _state) = network_host_server(&host).await;
    let response = server
        .build(trillium::Method::Query, "/sessions")
        .with_request_header(KnownHeaderName::Authorization, bearer.clone())
        .with_json_body(&serde_json::json!({"cwd": "/project", "connection": "test", "model": "m"}))
        .await;
    response.assert_status(200);
    assert_eq!(
        json(response.body())["effective"]["main"]["value"]["model"],
        "m"
    );
    assert!(
        host.ids().is_empty(),
        "a preview establishes no environment and mints nothing"
    );

    // Creation is mounted here, and validates the same way: a relative cwd is the
    // same `400` the socket answers, not a refusal for arriving over TCP. A
    // session is not minted — these tests stop before `boot::prepare`, which
    // would reach a provider.
    let created = server
        .post("/sessions")
        .with_request_header(KnownHeaderName::Authorization, bearer)
        .with_json_body(&serde_json::json!({
            "name": "fresh",
            "cwd": "relative",
            "model": "test/m",
            "env": {"HOME": "/h"},
        }))
        .await;
    assert_eq!(
        created.status(),
        400,
        "network creation: {}",
        created.body()
    );
    assert!(
        created.body().contains("cwd must be absolute"),
        "the same reason the socket gives: {}",
        created.body()
    );
    assert!(host.ids().is_empty(), "a rejected creation mints nothing");
    let _ = roots;
    host.swansong().shut_down().await;
}
