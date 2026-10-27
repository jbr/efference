use super::*;

/// The visible text of the hyperlink is the URL itself, so a terminal that
/// ignores OSC 8 still prints something copy-pasteable — which is the whole
/// point in the no-tty case, where the escape is least likely to be honored.
#[test]
fn an_osc8_link_still_reads_as_a_url_when_the_escape_is_stripped() {
    let url = "https://mcp.sentry.dev/oauth/authorize?code_challenge=abc&state=xyz";
    let rendered = osc8(url);
    assert!(rendered.contains(url));
    // Strip the escapes the way a dumb log capture effectively does.
    let stripped: String = rendered.replace('\x1b', "");
    assert!(stripped.contains(url), "{stripped}");
    // The URL appears as both link target and visible label.
    assert_eq!(rendered.matches(url).count(), 2);
}

/// A server that answers every request `401`, as one that wants authorization does.
fn unauthorized_server() -> std::net::SocketAddr {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let _ = stream.write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer\r\n\
                      Content-Length: 0\r\nConnection: close\r\n\r\n",
                );
            });
        }
    });
    addr
}

/// A preparation that refuses authorization fails the mount of a server asking for
/// it, naming why, and never enters the dance: nothing is told of a wait.
#[tokio::test]
async fn a_refused_authorization_fails_the_mount_without_the_dance() {
    let addr = unauthorized_server();
    let cwd = std::env::temp_dir();
    let mut toolbox = Toolbox::bash_and_mcps(cwd.clone(), None).unwrap();
    let servers = std::collections::BTreeMap::from([(
        "guarded".to_string(),
        crate::config::McpServerEntry {
            transport: crate::config::McpTransport::Http {
                url: format!("http://{addr}/mcp"),
            },
            optional: false,
        },
    )]);
    let store: Arc<dyn TokenStore> = Arc::new(crate::mcp::auth::store::KeychainStore::new(
        Arc::new(crate::secrets::Secrets::in_memory()),
    ));
    let waited = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let told = std::sync::Arc::clone(&waited);
    let authorization = AuthorizationHandling {
        refuse: true,
        waiting: Some(std::sync::Arc::new(move |_: Option<&str>| {
            told.store(true, std::sync::atomic::Ordering::SeqCst);
        })),
    };
    let error = mount_all(&mut toolbox, &servers, &cwd, &store, &[], &authorization)
        .await
        .expect_err("a required server that asks for authorization does not mount");
    let message = error.to_string();
    assert!(message.contains("`guarded`"), "{message}");
    assert!(message.contains("asks for authorization"), "{message}");
    assert!(message.contains("optional = true"), "{message}");
    assert!(!waited.load(std::sync::atomic::Ordering::SeqCst));
}
