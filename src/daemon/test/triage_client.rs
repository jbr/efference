//! A separate triage client's own readings on `GET /state` (docs/control-api.md):
//! `triage_upstream_failure`, `usage.triage_plan`, and `usage.triage_cold_streak`.

use super::runtime::{boot, started, workdir};
use super::*;
use crate::curation::triage::Triager;
use crate::frontend::SubmittedTurn;
use crate::inference::testing::upstream::{Canned, ScriptedUpstream, end_turn, tool_call};
use crate::record::log::Provider;
use trillium_tokio::Swansong;

fn state(session: &Session) -> Value {
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
    let main = ScriptedUpstream::new(|n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        _ => Canned::stream(end_turn()),
    });
    let triage = ScriptedUpstream::new(|_| {
        Canned::json(
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
        )
    });

    let (cwd, _dir) = workdir("triage-client");
    let mut boot = boot(&cwd);
    let endpoint =
        |upstream: &ScriptedUpstream| upstream.endpoint(Provider::Anthropic, "/v1/messages");
    boot.client = Client::new(endpoint(&main), "m", "sys");
    boot.triager = Triager::own(Client::new(endpoint(&triage), "t", "sys"));
    let host = Swansong::new();
    let runtime = started(boot, &host).await;
    let session = Arc::clone(runtime.session());

    let before = state(&session);
    assert!(before["triage_upstream_failure"].is_null());
    assert!(
        before["usage"]["triage_plan"].is_null(),
        "no plan window has been observed"
    );
    assert_eq!(before["usage"]["triage_cold_streak"], 0);

    session.submit_turn(SubmittedTurn::new("go", "s1"));
    // The failure cell is written by the triage client's own handler, which signals
    // nothing; the transport's next attempt, an arrival, follows the write.
    session
        .wait_until_with(&triage, "the triage client reports its 429", || {
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
    let (session, _rx) = Session::in_memory();
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
    let (session, _rx) = Session::in_memory();
    session.set_triage_client((
        upstream_failure_state(),
        crate::inference::api::plan_state(),
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
