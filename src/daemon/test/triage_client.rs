//! A separate triage client's own readings on `GET /state` (docs/control-api.md):
//! `triage_upstream_failure`, `usage.triage_plan`, and `usage.triage_cold_streak`.

use super::runtime::boot;
use super::upstream_hold::{Answer, end_turn, scripted, tool_call, wait_until};
use super::*;
use crate::daemon::SessionRuntime;
use crate::frontend::SubmittedTurn;
use crate::triage::Triager;
use std::sync::atomic::AtomicUsize;
use trillium_tokio::Swansong;

fn endpoint(addr: std::net::SocketAddr) -> crate::api::Endpoint {
    crate::api::Endpoint {
        provider: crate::api::Provider::Anthropic,
        url: Some(format!("http://{addr}/v1/messages")),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    }
}

fn state(session: &Session) -> serde_json::Value {
    serde_json::from_str(&session.state_json()).unwrap()
}

fn read_of(cache_read: u64) -> RequestUsage {
    RequestUsage::new(Usage {
        input_tokens: 1,
        output_tokens: 1,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: cache_read,
    })
    .with_model("triage-model")
}

/// **The triage client's failure is its own.** The agent's upstream answers; the
/// separate triage client's upstream answers 429. `triage_upstream_failure` carries the
/// status while the agent's `upstream_failure` stays clear.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_separate_triage_clients_429_shows_on_its_own_topic() {
    let main_requests = Arc::new(AtomicUsize::new(0));
    let main_addr = scripted(Arc::clone(&main_requests), |n| match n {
        0 => Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        _ => Answer::Stream(end_turn()),
    });
    let triage_requests = Arc::new(AtomicUsize::new(0));
    let triage_addr = scripted(Arc::clone(&triage_requests), |_| {
        Answer::Status(
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
        )
    });

    use std::hash::{BuildHasher, Hasher, RandomState};
    let nonce = RandomState::new().build_hasher().finish();
    let cwd = std::env::temp_dir().join(format!("efference-triage-client-{nonce:016x}"));
    std::fs::create_dir_all(cwd.join("scratch")).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let mut boot = boot(&cwd);
    boot.client = Client::new(endpoint(main_addr), "m", "sys", None);
    boot.triager = Triager::own(Client::new(endpoint(triage_addr), "t", "sys", None));
    let host = Swansong::new();
    let runtime = SessionRuntime::start(boot, &host);
    let session = Arc::clone(runtime.session());

    let before = state(&session);
    assert!(before["triage_upstream_failure"].is_null());
    assert!(
        before["usage"]["triage_plan"].is_null(),
        "no plan window has been observed"
    );
    assert_eq!(before["usage"]["triage_cold_streak"], 0);

    session.submit_turn(SubmittedTurn::new("go", "s1"));
    wait_until("the triage client reports its 429", || {
        !state(&session)["triage_upstream_failure"].is_null()
    })
    .await;
    let after = state(&session);
    assert_eq!(after["triage_upstream_failure"]["status"], 429);
    assert!(
        after["upstream_failure"].is_null(),
        "the agent's client was answered: {:?}",
        after["upstream_failure"]
    );
    host.shut_down().await;
}

/// A shared triage has no client of its own, so the three readings are null rather than
/// zero or a copy of the agent's.
#[test]
fn a_shared_triage_publishes_no_triage_readings() {
    let (session, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    session.record_triage(true, read_of(0));
    let v = state(&session);
    assert!(v["triage_upstream_failure"].is_null());
    assert!(v["usage"]["triage_plan"].is_null());
    assert!(v["usage"]["triage_cold_streak"].is_null());
}

/// The triage lineage's streak counts result triages and primers on the triage client:
/// the first read is a boot, an unexplained cold read adds one, a warm read clears, a
/// ping and a triage on the agent's client are not evidence, and the main streak does
/// not move.
#[test]
fn the_triage_lineage_has_its_own_cold_streak() {
    let (session, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    session.set_triage_client((
        crate::api::upstream_failure_state(),
        crate::api::plan_state(),
    ));
    let streak = |session: &Session| state(session)["usage"]["triage_cold_streak"].clone();
    assert_eq!(streak(&session), 0);

    session.record_request(RequestOrigin::TriagePrimer, read_of(0)); // boot
    assert_eq!(streak(&session), 0);
    session.record_triage(false, read_of(0));
    session.record_triage(false, read_of(0));
    assert_eq!(streak(&session), 2);

    session.record_triage_keepalive(read_of(0));
    session.record_triage(true, read_of(0)); // the agent's client: another lineage
    assert_eq!(streak(&session), 2, "neither is evidence about this prefix");

    session.record_triage(false, read_of(7_000));
    assert_eq!(streak(&session), 0, "a warm read clears it");
    assert_eq!(
        session.cold_streak(),
        0,
        "the main streak is a separate count"
    );
}
