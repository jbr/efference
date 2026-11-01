use super::*;
use crate::inference::testing::upstream::{Canned, ScriptedUpstream};

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

/// An upstream that answers every request `401`, as a server that wants authorization
/// does. The MCP client is resolved from the server's configured URL, so the upstream is
/// reached over a loopback port (`ScriptedUpstream::bind`); it closes the port when it
/// drops. Returns it with its origin.
fn unauthorized_server() -> (ScriptedUpstream, String) {
    let upstream = ScriptedUpstream::new(|_| {
        Canned::json(401, "")
            .without_content_type()
            .with_header("www-authenticate", "Bearer")
    });
    let origin = upstream.bind();
    (upstream, origin)
}

/// A preparation that refuses authorization fails the mount of a server asking for
/// it, naming why, and never enters the dance: nothing is told of a wait.
#[tokio::test]
async fn a_refused_authorization_fails_the_mount_without_the_dance() {
    let (_server, origin) = unauthorized_server();
    let cwd = std::env::temp_dir();
    let mut toolbox = Toolbox::bash_and_mcps(cwd.clone(), None).unwrap();
    let servers = std::collections::BTreeMap::from([(
        "guarded".to_string(),
        crate::config::McpServerEntry {
            transport: crate::config::McpTransport::Http {
                url: format!("{origin}/mcp"),
            },
            optional: false,
        },
    )]);
    let store: Arc<dyn TokenStore> = Arc::new(auth::store::KeychainStore::new(Arc::new(
        crate::platform::secrets::Secrets::in_memory(),
    )));
    let waited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let told = Arc::clone(&waited);
    let authorization = AuthorizationHandling {
        refuse: true,
        waiting: Some(Arc::new(move |_: Option<&str>| {
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

/// A token store whose every operation fails, as one over a secret store that cannot
/// be opened does (docs/secrets.md). Counts the reads it is asked for.
#[derive(Default)]
struct UnusableStore {
    loads: std::sync::atomic::AtomicUsize,
}

impl TokenStore for UnusableStore {
    fn load(&self, _: &str) -> anyhow::Result<Option<auth::store::StoredAuth>> {
        self.loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        anyhow::bail!("the secret store could not be opened")
    }
    fn save(&self, _: &str, _: &auth::store::StoredAuth) -> anyhow::Result<()> {
        anyhow::bail!("the secret store could not be opened")
    }
    fn clear(&self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("the secret store could not be opened")
    }
    fn refresh_lock(&self, _: &str) -> Arc<tokio::sync::Mutex<()>> {
        Arc::default()
    }
}

/// The smallest stdio MCP server the client negotiates with: it declines
/// `server/discover`, answers `initialize`, and lists one tool. Written into `dir`.
fn stub_stdio_server(dir: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("stub-mcp.sh");
    std::fs::write(
        &script,
        r#"while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | grep -o '"id":[0-9]*' | head -n 1 | cut -d: -f2)
  case "$line" in
    *'"method":"server/discover"'*)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id" ;;
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"stub","version":"0"}}}\n' "$id" ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"ping","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
  esac
done
"#,
    )
    .unwrap();
    script
}

/// A stdio server is sent no grant, so its mount reads nothing from the store: on a
/// machine whose secret store fails every operation it still mounts. An HTTP server
/// beside it is where the read happens, and fails, naming the store's complaint.
#[tokio::test]
async fn a_stdio_server_mounts_when_the_secret_store_fails_every_operation() {
    let dir = std::env::temp_dir().join(format!("eff-stdio-mount-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = stub_stdio_server(&dir);
    let mut toolbox = Toolbox::bash_and_mcps(dir.clone(), None).unwrap();
    let stdio = std::collections::BTreeMap::from([(
        "local".to_string(),
        crate::config::McpServerEntry {
            transport: crate::config::McpTransport::Stdio {
                command: "sh".into(),
                args: vec![script.display().to_string()],
                env: Default::default(),
            },
            optional: false,
        },
    )]);
    let unusable = Arc::new(UnusableStore::default());
    let store: Arc<dyn TokenStore> = unusable.clone();
    let authorization = AuthorizationHandling::default();

    let unmounted = mount_all(&mut toolbox, &stdio, &dir, &store, &[], &authorization)
        .await
        .expect("a stdio server mounts without the store");
    assert!(unmounted.is_empty());
    assert!(
        toolbox
            .definitions()
            .iter()
            .any(|d| d["name"] == "local__ping")
    );
    assert_eq!(unusable.loads.load(std::sync::atomic::Ordering::SeqCst), 0);

    let (_server, origin) = unauthorized_server();
    let http = std::collections::BTreeMap::from([(
        "remote".to_string(),
        crate::config::McpServerEntry {
            transport: crate::config::McpTransport::Http {
                url: format!("{origin}/mcp"),
            },
            optional: false,
        },
    )]);
    let error = mount_all(&mut toolbox, &http, &dir, &store, &[], &authorization)
        .await
        .expect_err("an HTTP server's grant is read, and the read fails");
    let message = error.to_string();
    assert!(message.contains("could not be opened"), "{message}");
    assert_eq!(unusable.loads.load(std::sync::atomic::Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
