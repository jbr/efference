use super::*;
use crate::daemon::{App, Host, Session, Transport, handler, http::control_routes};
use crate::log::{AssistantOutput, ContentBlock, Exclusion, Payload};
use crate::persist::Log;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use trillium_testing::TestServer;

const ARTIFACT_SOURCE: &str = concat!(
    "```rust\nlet ordinary = true;\n```\n",
    "```html-artifact preview\nnot an artifact\n```\n",
    "```html-artifact\n<main>ready</main>\n```\n",
    "```html-artifact\n<main>unfinished</main>\n",
);

fn session() -> Arc<Session> {
    Session::new(std::env::current_dir().unwrap(), Log::in_memory()).0
}

fn assistant(text: Vec<&str>, excluded: Option<Exclusion>) -> Payload {
    Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(
            Vec::new(),
            text.into_iter()
                .map(|text| ContentBlock::Text(text.to_string()))
                .collect(),
            Vec::new(),
        ),
        excluded,
    }
}

async fn resolver_server(shared: &Arc<Session>) -> TestServer<impl trillium::Handler> {
    let shared = Arc::clone(shared);
    TestServer::new((
        trillium::Init::new(move |info: trillium::Info| {
            let shared = Arc::clone(&shared);
            async move { info.with_shared_state(shared) }
        }),
        control_routes(),
    ))
    .await
}

async fn host_web_server() -> TestServer<impl trillium::Handler> {
    use crate::auth::Root;
    use std::hash::{BuildHasher, Hasher, RandomState};

    let nonce = RandomState::new().build_hasher().finish();
    let root = std::env::temp_dir().join(format!("efference-artifact-host-{nonce:016x}"));
    std::fs::create_dir_all(&root).expect("scratch root");
    let (auth, _listener) = crate::daemon::tcp::arm_with_state(
        crate::daemon::tcp::TcpBind {
            addr: "127.0.0.1:0".into(),
            tls: None,
            public_url: "http://localhost:7777".into(),
            root: Root::new(root.to_string_lossy().as_ref()),
            open: false,
        },
        root.join("state.db"),
    );
    let host = Host::new(
        crate::inference::InferenceRegistry::empty(),
        crate::env_table::EnvSnapshot::new(std::collections::BTreeMap::new())
            .expect("an empty environment is valid"),
    );
    TestServer::new((
        trillium::Init::new(move |info: trillium::Info| {
            let host = Arc::clone(&host);
            async move { info.with_shared_state(host) }
        }),
        handler(Transport::Web { auth }, App::Host),
    ))
    .await
    .with_host("localhost")
}

async fn loopback_get(port: u16, path: &str) -> io::Result<String> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await?;
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        stream.read_to_end(&mut response),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "loopback response did not close"))??;
    Ok(String::from_utf8_lossy(&response).into_owned())
}

fn response_parts(response: &str) -> (&str, &str) {
    response
        .split_once("\r\n\r\n")
        .expect("HTTP response has a header/body separator")
}

#[tokio::test]
async fn resolver_tokens_and_loopback_surface_follow_the_artifact_contract() {
    // Keep every resolver and loopback request inside this one Tokio runtime: the
    // production loopback listener is initialized once and spawned on its runtime.
    let first = session();
    assert_eq!(
        first
            .append(
                1,
                assistant(
                    vec![
                        ARTIFACT_SOURCE,
                        "```html-artifact\n<aside>second block</aside>\n```\n"
                    ],
                    None
                )
            )
            .unwrap(),
        0
    );
    first
        .append(
            1,
            Payload::User(vec![ContentBlock::Text(ARTIFACT_SOURCE.to_string())]),
        )
        .unwrap();
    first
        .append(1, assistant(vec![ARTIFACT_SOURCE], Some(Exclusion::Empty)))
        .unwrap();

    let server = resolver_server(&first).await;
    for (path, status) in [
        ("/v1/artifacts/not-a-seq/0/2", 400),
        ("/v1/artifacts/99/0/2", 404),
        ("/v1/artifacts/1/0/2", 404),
        ("/v1/artifacts/2/0/2", 404),
        ("/v1/artifacts/0/99/2", 404),
        ("/v1/artifacts/0/0/0", 404),
        ("/v1/artifacts/0/0/1", 404),
        ("/v1/artifacts/0/0/3", 404),
        ("/v1/artifacts/0/0/99", 404),
    ] {
        server.get(path).await.assert_status(status);
    }

    let first_resolution = server.get("/v1/artifacts/0/0/2").await;
    first_resolution.assert_status(200);
    let first_resolution: serde_json::Value =
        serde_json::from_str(first_resolution.body()).unwrap();
    let first_url = first_resolution["url"].as_str().unwrap();
    let first_token = first_url
        .strip_prefix("/artifacts/")
        .unwrap()
        .strip_suffix('/')
        .unwrap();
    assert_eq!(first_token.len(), 43);
    assert!(
        first_token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    );

    let host = host_web_server().await;
    let host_document = host.get(first_url).await;
    host_document.assert_status(200);
    assert!(host_document.body().contains("<main>ready</main>"));
    assert!(host_document.body().contains(RUNTIME_TAG));
    assert_eq!(
        host_document
            .response_headers()
            .get_str(KnownHeaderName::ContentSecurityPolicy),
        Some(ARTIFACT_CSP)
    );
    assert_eq!(
        host_document.response_headers().get_str("x-frame-options"),
        Some("SAMEORIGIN")
    );
    let host_asset = host
        .get(&format!("/artifacts/{first_token}/_efference/runtime.js"))
        .await;
    host_asset.assert_status(200);
    assert_eq!(host_asset.body(), RUNTIME_JS);
    // Only the token document and assets are root routes. Resolution stays under a
    // session's control router, and token-shaped session paths are not capabilities.
    host.get("/v1/artifacts/0/0/2").await.assert_status(401);
    host.get(&format!("/sessions/work{first_url}"))
        .await
        .assert_status(401);

    let repeated = server.get("/v1/artifacts/0/0/2").await;
    repeated.assert_status(200);
    let repeated: serde_json::Value = serde_json::from_str(repeated.body()).unwrap();
    assert_eq!(repeated["url"].as_str(), Some(first_url));

    let second = session();
    second
        .append(1, assistant(vec![ARTIFACT_SOURCE], None))
        .unwrap();
    let second_server = resolver_server(&second).await;
    let scoped = second_server.get("/v1/artifacts/0/0/2").await;
    scoped.assert_status(200);
    let scoped: serde_json::Value = serde_json::from_str(scoped.body()).unwrap();
    assert_ne!(scoped["url"].as_str(), Some(first_url));

    // The external URL points only at the artifact listener. It serves the token
    // document and embedded web-owned resources, with no control API mounted.
    let external_url = first_resolution["external_url"].as_str().unwrap();
    let external = external_url
        .strip_prefix("http://127.0.0.1:")
        .expect("external_url is loopback-only");
    let (port, _) = external.split_once('/').unwrap();
    let port = port.parse::<u16>().unwrap();
    let document = loopback_get(port, first_url).await.unwrap();
    let (headers, body) = response_parts(&document);
    assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
    let headers_lower = headers.to_ascii_lowercase();
    assert!(headers_lower.contains("content-security-policy: "));
    assert!(headers_lower.contains("frame-ancestors 'self'"));
    assert!(headers_lower.contains("x-frame-options: sameorigin"));
    assert!(!headers_lower.contains("x-frame-options: deny"));
    assert!(headers_lower.contains("referrer-policy: no-referrer"));
    assert!(headers_lower.contains("x-content-type-options: nosniff"));
    assert!(body.contains("<main>ready</main>"));
    assert!(body.contains(RUNTIME_TAG));

    let runtime = loopback_get(
        port,
        &format!("/artifacts/{first_token}/_efference/runtime.js"),
    )
    .await
    .unwrap();
    let (runtime_headers, runtime_body) = response_parts(&runtime);
    assert!(
        runtime_headers.starts_with("HTTP/1.1 200"),
        "{runtime_headers}"
    );
    assert!(
        runtime_headers
            .to_ascii_lowercase()
            .contains("content-type: text/javascript; charset=utf-8")
    );
    let runtime_headers_lower = runtime_headers.to_ascii_lowercase();
    assert!(runtime_headers_lower.contains("referrer-policy: no-referrer"));
    assert!(runtime_headers_lower.contains("x-content-type-options: nosniff"));
    assert_eq!(runtime_body, RUNTIME_JS);

    let theme = loopback_get(
        port,
        &format!("/artifacts/{first_token}/_efference/theme.css"),
    )
    .await
    .unwrap();
    let (theme_headers, theme_body) = response_parts(&theme);
    assert!(theme_headers.starts_with("HTTP/1.1 200"), "{theme_headers}");
    assert!(
        theme_headers
            .to_ascii_lowercase()
            .contains("content-type: text/css; charset=utf-8")
    );
    let theme_headers_lower = theme_headers.to_ascii_lowercase();
    assert!(theme_headers_lower.contains("referrer-policy: no-referrer"));
    assert!(theme_headers_lower.contains("x-content-type-options: nosniff"));
    assert_eq!(theme_body, THEME_CSS);

    let control = loopback_get(port, "/v1/state").await.unwrap();
    assert!(response_parts(&control).0.starts_with("HTTP/1.1 404"));
}
