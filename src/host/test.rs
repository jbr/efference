//! Tests for the host (docs/host.md): the discovery scan over real session dirs, the
//! proxy round-trip against a stub session daemon on a real Unix socket, the gate on
//! the host's own surface, and the web client at the root.
//!
//! The stub is bound on an actual UDS rather than the in-memory transport the other
//! daemon tests use, because the socket *is* what the proxy addresses.

use super::*;
use crate::auth::{BurnLedger, Root};
use crate::daemon::tcp::{TlsPem, arm_with_ledger};
use std::time::{Duration, Instant};
use trillium::{Conn, KnownHeaderName, Method};
use trillium_router::Router;
use trillium_testing::TestServer;

/// A throwaway burn-ledger path, so tests never write the real user-scoped file.
fn scratch_ledger() -> BurnLedger {
    BurnLedger::new(std::env::temp_dir().join(format!("efference-host-test-{}", nonce())))
}

/// A unique-ish 16-hex name component, without pulling `rand` into a test.
fn nonce() -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};
    format!("{:016x}", RandomState::new().build_hasher().finish())
}

/// The credential half of a plaintext-loopback bind, under a per-test root.
fn armed(root: &str) -> Arc<TcpAuth> {
    let (auth, _listener) = arm_with_ledger(
        TcpBind {
            addr: "127.0.0.1:0".into(),
            tls: None::<TlsPem>,
            host: "localhost".into(),
            root: Root::new(root),
            open: false,
        },
        scratch_ledger(),
    );
    auth
}

/// A scratch session base. Bound under `/tmp` rather than `temp_dir()`: Darwin caps
/// Unix-socket pathnames at 104 bytes and the harness's `$TMPDIR` is long.
fn scratch_base() -> PathBuf {
    let base = PathBuf::from("/tmp").join(format!("effh-{}", nonce()));
    std::fs::create_dir_all(&base).expect("scratch base");
    base
}

/// A stub session daemon on a real socket inside `base/<id>/`, answering the two
/// shapes the proxy has to carry: a plain JSON read, and a stream that arrives in
/// pieces over time.
fn spawn_stub_session(base: &Path, id: &str) {
    let dir = base.join(id);
    std::fs::create_dir_all(&dir).expect("session dir");
    let socket = dir.join(SOCKET_NAME);
    let _ = std::fs::remove_file(&socket);
    trillium_tokio::config()
        .without_signals()
        .listeners()
        .bind_uds(&socket)
        .expect("bind stub uds")
        .spawn(stub_router());
}

/// The gap between streamed events, and the window a non-buffering proxy must beat.
const TICK: Duration = Duration::from_millis(250);
const TICKS: u32 = 3;

fn stub_router() -> Router {
    Router::new()
        .get("/v1/state", |conn: Conn| async move {
            // Echo the query so a test can prove it made the trip unmangled.
            let q = conn.querystring().to_string();
            conn.with_status(200)
                .with_body(format!(r#"{{"last_seq":3,"q":"{q}"}}"#))
                .halt()
        })
        .post("/v1/turns", |mut conn: Conn| async move {
            let body = conn.request_body_string().await.unwrap_or_default();
            conn.with_status(202).with_body(body).halt()
        })
        .get(
            "/v1/events",
            // The first event goes out immediately; the rest arrive a tick apart, so
            // a proxy that buffered the body could not answer for TICKS × TICK.
            trillium_sse::sse(|_: &mut Conn| {
                Box::pin(futures_lite::stream::unfold(0u32, |i| async move {
                    if i >= TICKS {
                        return None;
                    }
                    if i > 0 {
                        tokio::time::sleep(TICK).await;
                    }
                    Some((format!("tick {i}"), i + 1))
                }))
            }),
        )
        .get("/v1/log", |conn: Conn| async move {
            conn.with_status(404)
                .with_body(r#"{"code":"not_found","message":"the session said so"}"#)
                .halt()
        })
}

/// The host stack over a scratch base, virtually hosted at the bind's public name so
/// the gate's Host check sees what a real conn carries.
async fn host_server(auth: &Arc<TcpAuth>, base: PathBuf) -> TestServer<impl trillium::Handler> {
    TestServer::new(stack(Arc::clone(auth), base))
        .await
        .with_host("localhost")
}

fn bearer(auth: &TcpAuth) -> String {
    format!("Bearer {}", auth.bearer())
}

/// Discovery is the liveness scan promoted to an endpoint: a dir with a listening
/// daemon is listed, a dir whose socket file has no listener behind it is not, and a
/// dir with no socket at all is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_lists_live_sessions_only() {
    let base = scratch_base();
    spawn_stub_session(&base, "live-one");
    spawn_stub_session(&base, "live-two");
    // A stale dir from a dead daemon: the socket file exists, nothing is listening.
    std::fs::create_dir_all(base.join("dead")).unwrap();
    std::fs::write(base.join("dead").join(SOCKET_NAME), b"").unwrap();
    // A session dir that never got as far as binding.
    std::fs::create_dir_all(base.join("no-socket")).unwrap();

    let auth = armed("host-discovery");
    let server = host_server(&auth, base).await;
    let conn = server
        .get("/sessions")
        .with_request_header(KnownHeaderName::Authorization, bearer(&auth))
        .await;
    conn.assert_status(200);
    let listed: Vec<SessionEntry> = serde_json::from_str(conn.body()).expect("a session array");
    assert_eq!(
        listed,
        vec![
            SessionEntry {
                id: "live-one".into()
            },
            SessionEntry {
                id: "live-two".into()
            }
        ]
    );
}

/// The proxy carries the session's own API unchanged: path, query, method, request
/// body, status, and the session's error envelope on a `404` it produced itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_proxy_round_trips_a_session_request() {
    let base = scratch_base();
    spawn_stub_session(&base, "work");
    let auth = armed("host-proxy");
    let server = host_server(&auth, base).await;

    let state = server
        .get("/sessions/work/v1/state?wait=settled")
        .with_request_header(KnownHeaderName::Authorization, bearer(&auth))
        .await;
    state.assert_status(200);
    assert!(state.body().contains(r#""last_seq":3"#), "{}", state.body());
    assert!(
        state.body().contains(r#""q":"wait=settled""#),
        "the query reached the session: {}",
        state.body()
    );

    let turn = server
        .post("/sessions/work/v1/turns")
        .with_request_header(KnownHeaderName::Authorization, bearer(&auth))
        .with_json_body(&serde_json::json!({ "text": "hello" }))
        .await;
    turn.assert_status(202);
    assert!(turn.body().contains("hello"), "{}", turn.body());

    // A `404` the session produced is forwarded with its body, not swallowed and
    // re-answered by the host's own frontend handler.
    let missing = server
        .get("/sessions/work/v1/log")
        .with_request_header(KnownHeaderName::Authorization, bearer(&auth))
        .await;
    missing.assert_status(404);
    assert!(
        missing.body().contains("the session said so"),
        "{}",
        missing.body()
    );
}

/// A stream flushes through: the response is answerable long before the session has
/// finished producing it. A buffering proxy would not answer until the last event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_stream_is_flushed_not_buffered() {
    let base = scratch_base();
    spawn_stub_session(&base, "streaming");
    let auth = armed("host-stream");
    let handler = stack(Arc::clone(&auth), base);

    let started = Instant::now();
    let mut conn =
        trillium_testing::TestConn::build(Method::Get, "/sessions/streaming/v1/events", ())
            .with_request_header(KnownHeaderName::Host, "localhost")
            .with_request_header(KnownHeaderName::Authorization, bearer(&auth))
            .run_async(&handler)
            .await;
    let headers_at = started.elapsed();
    assert_eq!(conn.status().map(|s| s as u16), Some(200));
    assert!(
        headers_at < TICK,
        "the response waited on the whole stream ({headers_at:?})"
    );
    // The body is a stream with no declared length — the shape a buffered copy could
    // not have — and it does eventually carry every event.
    let body = conn.take_response_body().expect("a response body");
    assert_eq!(body.len(), None, "the proxied body declares a length");
    assert!(body.is_streaming(), "the proxied body is not a stream");
    let read = String::from_utf8(body.into_bytes().await.unwrap().into_owned()).unwrap();
    for i in 0..TICKS {
        assert!(read.contains(&format!("tick {i}")), "{read:?}");
    }
}

/// An id that names no live session is a `404` naming the recovery, whether the dir
/// is absent, holds no socket, or holds a stale one. Path-shaped ids never reach the
/// filesystem join.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_session_is_not_found() {
    let base = scratch_base();
    std::fs::create_dir_all(base.join("dead")).unwrap();
    std::fs::write(base.join("dead").join(SOCKET_NAME), b"").unwrap();
    let auth = armed("host-unknown");
    let server = host_server(&auth, base).await;

    // Dot segments never arrive: URL parsing removes them before the request goes
    // out, and the id validation that would refuse one anyway is pinned below.
    for id in ["absent", "dead", "not a session"] {
        let conn = server
            .get(&format!("/sessions/{id}/v1/state"))
            .with_request_header(KnownHeaderName::Authorization, bearer(&auth))
            .await;
        conn.assert_status(404);
        assert!(
            conn.body().contains("no live session"),
            "{id}: {}",
            conn.body()
        );
    }
}

/// The host's guarded surface is `/sessions` — the listing and every proxied path
/// alike. The web client's shell stays anonymous, as it is on the session bind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_surface_is_guarded_and_the_client_is_not() {
    let base = scratch_base();
    spawn_stub_session(&base, "work");
    let auth = armed("host-gate");
    let server = host_server(&auth, base).await;

    for path in ["/sessions", "/sessions/work/v1/state"] {
        server
            .get(path)
            .await
            .assert_status(401)
            .assert_body_contains(r#""code":"unauthorized""#);
    }

    // The cookie the login view mints opens the same surface the bearer does.
    // The current code, read back out of the bootstrap link rather than re-derived:
    // the link is the surface the operator uses, so the test uses it too.
    let link = auth.mint_link();
    let code = link.rsplit("#token=").next().unwrap().to_string();
    let session = server
        .post("/auth/session")
        .with_json_body(&serde_json::json!({ "code": code }))
        .await;
    session.assert_status(200);
    let cookie = session
        .header(KnownHeaderName::SetCookie)
        .expect("a session cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string();
    server
        .get("/sessions")
        .with_request_header(KnownHeaderName::Cookie, cookie)
        .await
        .assert_status(200);

    // The SPA shell and its assets are anonymous, and an unmatched path is a 404
    // rather than a scanner-probe 200.
    server
        .get("/")
        .await
        .assert_status(200)
        .assert_header_with(KnownHeaderName::ContentType, |ct| {
            assert!(ct.as_str().is_some_and(|ct| ct.starts_with("text/html")))
        });
    server.get("/nonexistent").await.assert_status(404);
}

/// Ids are validated before they are joined onto a path: only a plain segment names
/// a session dir.
#[test]
fn only_plain_ids_resolve_to_a_socket() {
    let sessions = Sessions::new(PathBuf::from("/base"));
    assert_eq!(
        sessions.socket("work-1.2"),
        Some(PathBuf::from("/base/work-1.2").join(SOCKET_NAME))
    );
    for bad in ["", ".", "..", "a/b", "/etc", "a b", "wörk"] {
        assert_eq!(sessions.socket(bad), None, "{bad:?} resolved to a socket");
    }
}
