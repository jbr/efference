use super::*;
use crate::platform::auth::Root;
use std::collections::BTreeMap;

/// A TCP bind that fails ends the host instead of leaving it serving the control
/// socket alone: the control socket is already serving when the TCP bind runs, so
/// `serve_host` shuts the host's `Swansong` down and returns the bind error
/// (docs/control-channel.md).
#[tokio::test]
async fn a_failed_tcp_bind_shuts_the_host_down() {
    // Darwin limits a socket pathname to `SUN_LEN` (104 bytes), and the per-session
    // `$TMPDIR` can take most of that, so the control socket goes under a short scratch
    // directory of its own in `/tmp`. Not `/tmp` itself: the host tightens the
    // directory it binds in.
    let short = tempfile::TempDir::with_prefix_in("eff-srv-", "/tmp").expect("a scratch dir");
    let state = crate::platform::testing::temp_dir("serve-bind");
    let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let addr = taken.local_addr().expect("a bound address").to_string();
    let env = crate::config::env_table::EnvSnapshot::new(BTreeMap::new())
        .expect("an empty environment is valid");
    let host = Host::new(crate::inference::InferenceRegistry::empty(), env);

    let served = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        serve_host_with_state(
            Arc::clone(&host),
            short.path().join("h.sock"),
            Some(TcpBind {
                addr: addr.clone(),
                tls: None,
                public_url: "http://localhost".into(),
                root: Root::new("serve-bind"),
                open: false,
            }),
            state.path().join("state.db"),
        ),
    )
    .await
    .expect("serve_host returns on a failed bind rather than serving on");

    let error = served.expect_err("the port is taken").to_string();
    assert!(
        error.contains(&format!("failed to bind {addr}")),
        "the error names the bind: {error}"
    );
    // Begun is what ends the caller's tasks under the `Swansong`: their interrupts stop.
    assert!(
        !host.swansong().state().is_running(),
        "the host's shutdown has begun"
    );
    // The control socket's server, spawned before the TCP bind failed, winds down too.
    tokio::time::timeout(std::time::Duration::from_secs(30), host.swansong().clone())
        .await
        .expect("the host's shutdown completes");
    drop(taken);
}
