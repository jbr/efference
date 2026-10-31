//! Tests for the triage lineage's keepalive (docs/transport.md): where it arms, what it
//! stores, and what its ping sends — the stored request, as sent.

use super::keepalive::{self, pinged_session, record_turn};
use super::runtime::{started, workdir};
use super::*;
use crate::api::{Client, Endpoint, PromptCachePolicy, RequestUsage, SentRequest};
use crate::daemon::SessionRuntime;
use crate::daemon::keepalive::{CacheLineage, KeepaliveSpec, keepalive_interval};
use crate::frontend::SubmittedTurn;
use crate::log::{Provider, Summary, Timestamp};
use crate::project::WireTurn;
use crate::testing::upstream::{Canned, Request, ScriptedUpstream, end_turn, tool_call};
use crate::triage::Triager;
use tempfile::TempDir;
use trillium_tokio::Swansong;

/// A `usage` with `prefix` prompt tokens, for recording a request by hand.
fn prompt_usage(prefix: u64) -> RequestUsage {
    RequestUsage::new(Usage {
        input_tokens: prefix,
        ..Usage::default()
    })
}

/// The Messages endpoint `upstream` answers on.
fn messages_endpoint(upstream: &ScriptedUpstream) -> Endpoint {
    upstream.endpoint(Provider::Anthropic, "/v1/messages")
}

/// The triage tools and forced `retain` a stored request carries.
fn stored(turns: Vec<WireTurn>) -> SentRequest {
    SentRequest::new(
        turns,
        vec![
            serde_json::json!({"name": "retain"}),
            serde_json::json!({"name": "triage"}),
        ],
        ToolChoice::force("retain"),
    )
}

/// The triage lineage's ping is the stored request (docs/transport.md): its turns, its
/// `tools[]` and its forced `retain`, and nothing appended. The loop's own spec carries no
/// envelope for this lineage. Taken on a paused clock, with the ping due at once: the
/// interval is read off `std::time::Instant`, which a paused clock does not move.
#[tokio::test(start_paused = true)]
async fn a_triage_ping_sends_the_stored_request() {
    let ok = r#"{"id":"m","model":"m","usage":{"input_tokens":5000,"output_tokens":1}}"#;
    let upstream = ScriptedUpstream::new(move |_| Canned::json(200, ok));
    let (shared, _main) = pinged_session(&upstream, 5_000);
    let request = stored(vec![WireTurn::text("the triage's history")]);
    shared.store_triage_request(request, Timestamp::now(), &prompt_usage(5_000));
    shared.record_triage(false, prompt_usage(5_000));
    let spec = KeepaliveSpec {
        lineage: CacheLineage::Triage,
        client: Arc::new(Client::new(
            messages_endpoint(&upstream),
            "claude-haiku-5",
            "sys",
            None,
        )),
        tools: Vec::new(),
        ping_at: Duration::ZERO,
        prices: None,
    };
    let task = tokio::spawn(keepalive_loop(spec, Arc::clone(&shared)));
    shared
        .wait_until_with(&upstream, "the ping", || upstream.count() == 1)
        .await;
    task.abort();

    let body = upstream.requests()[0].json();
    assert_eq!(
        body["tool_choice"],
        serde_json::json!({"type": "tool", "name": "retain", "disable_parallel_tool_use": true}),
        "the forced choice the triage sent, parallel calls off"
    );
    let tools: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["retain", "triage"]);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "the stored turns, and nothing appended");
    assert!(messages[0].to_string().contains("the triage's history"));
}

/// The triage keepalive is armed on an own triage model whose client reports a prompt
/// cache, on any dialect (docs/transport.md). A shared triage rides the main prefix and
/// arms none, and neither does a model reporting no cache. The configured interval
/// applies to both lineages, and zero turns the triage one off.
#[test]
fn the_triage_keepalive_arms_on_an_own_model_with_a_prompt_cache() {
    let messages = |policy| {
        Triager::own(
            Client::new(
                Endpoint::at(Provider::Anthropic, "http://127.0.0.1:9/v1/messages"),
                "claude-haiku-5",
                "sys",
                None,
            )
            .with_prompt_cache(policy),
        )
    };
    assert!(KeepaliveSpec::triage(&Triager::shared(), None, None).is_none());
    assert!(KeepaliveSpec::triage(&messages(PromptCachePolicy::None), None, None).is_none());
    let armed = KeepaliveSpec::triage(&messages(PromptCachePolicy::Explicit), None, None)
        .expect("a Messages triage model with a cache is kept warm");
    assert_eq!(armed.lineage, CacheLineage::Triage);
    assert!(
        armed.tools.is_empty(),
        "its stored request carries the tools"
    );
    assert_eq!(
        armed.ping_at,
        keepalive_interval(Duration::from_secs(5 * 60))
    );

    let codex = Endpoint::at(
        Provider::OpenAICodex,
        "http://127.0.0.1:9/backend-api/codex/responses",
    );
    let triage = Triager::own(
        Client::new(codex, "gpt-6.1-sol", "sys", None)
            .with_prompt_cache(PromptCachePolicy::Implicit),
    );
    let armed = KeepaliveSpec::triage(&triage, None, None)
        .expect("an implicit Responses lineage is kept warm");
    assert_eq!(
        armed.ping_at,
        keepalive_interval(Duration::from_secs(10 * 60))
    );
    let off = KeepaliveSpec::triage(&triage, Some(Duration::ZERO), None).unwrap();
    assert!(off.ping_at.is_zero());
}

/// A compaction moves the prefix the stored request read. It stops being re-sent, and the
/// triage clock's compaction filter empties the ping's prefix, as it does the main loop's.
#[tokio::test]
async fn a_fold_drops_the_stored_request() {
    let (shared, _rx) = Session::in_memory();
    shared.store_triage_request(
        stored(vec![WireTurn::text("input")]),
        Timestamp::now(),
        &prompt_usage(5_000),
    );
    shared.record_request(RequestOrigin::TriagePrimer, prompt_usage(5_000));
    assert!(shared.ping_request(CacheLineage::Triage, &[]).is_some());
    tokio::time::sleep(Duration::from_millis(5)).await;
    shared
        .append(1, Payload::Summary(Summary::consolidation(0, 0, "account")))
        .unwrap();
    assert_eq!(shared.ping_request(CacheLineage::Triage, &[]), None);
    assert_eq!(shared.ping_prefix_tokens(CacheLineage::Triage), None);
    assert_eq!(shared.keepalive_clocks(CacheLineage::Triage).0, None);
}

/// The stored request's prompt tokens stand for what a ping reads until a ping measures
/// it; the ping's own prompt tokens replace them.
#[tokio::test]
async fn a_triage_ping_measures_the_stored_prefix() {
    let (shared, _rx) = Session::in_memory();
    shared.store_triage_request(
        stored(vec![WireTurn::text("input")]),
        Timestamp::now(),
        &prompt_usage(7_000),
    );
    assert_eq!(shared.ping_prefix_tokens(CacheLineage::Triage), Some(7_000));
    shared.record_triage_keepalive(prompt_usage(5_000));
    assert_eq!(shared.ping_prefix_tokens(CacheLineage::Triage), Some(5_000));
}

/// Idle triage runs on the agent's client, so it leaves the triage clock alone. A triage
/// ping stamps that clock and not the real-turn clock, so it cannot reset the give-up cap.
#[tokio::test]
async fn idle_triage_is_no_touch_and_a_triage_ping_keeps_the_cap() {
    let (shared, _rx) = Session::in_memory();
    shared.record_request(RequestOrigin::Triage, prompt_usage(1_000));
    assert_eq!(shared.keepalive_clocks(CacheLineage::Triage).0, None);

    record_turn(&shared, 1_000);
    tokio::time::sleep(Duration::from_millis(20)).await;
    shared.record_triage_keepalive(prompt_usage(1_000));
    let (touch, turn) = shared.keepalive_clocks(CacheLineage::Triage);
    assert!(touch.is_some(), "the ping stamps the triage clock");
    assert!(
        turn.is_some_and(|t| t >= Duration::from_millis(20)),
        "the real-turn clock still reads the turn, not the ping"
    );
}

// ---- a separate Messages triage model, end to end ----

/// The model name the separate triage client sends, which tells its requests apart.
const TRIAGE_MODEL: &str = "triage-m";

/// The interval the end-to-end sessions run their keepalive at. The clocks it is read
/// against are `std::time::Instant`s, which a paused clock does not move, so a test sets
/// them back (`keepalive::idle_for`) instead of waiting an interval out.
const PING_AT: Duration = Duration::from_secs(10 * 60);

/// A ping's non-streamed answer.
const PING: &str = r#"{"id":"p","model":"m","usage":{"input_tokens":10,"cache_read_input_tokens":900,"output_tokens":0}}"#;

/// The refusal a model that does not take a forced `tool_choice` answers with.
const FORCED_REFUSAL: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"tool_choice: type \"tool\" is not supported for this model."}}"#;

/// A session whose triage runs on a separate Messages model reporting a cache, against
/// `upstream`, at [`PING_AT`], returned once its loop has gone idle. The main model
/// reports none, so the main keepalive is off and every request after the turn's is the
/// triage lineage's.
async fn start_with_triage_model(
    upstream: &ScriptedUpstream,
) -> (Swansong, SessionRuntime, TempDir) {
    let (cwd, dir) = workdir("triage-keepalive");
    let mut boot = runtime::boot(&cwd);
    boot.client = Client::new(messages_endpoint(upstream), "m", "sys", None);
    boot.triager = Triager::own(
        Client::new(
            messages_endpoint(upstream),
            TRIAGE_MODEL,
            "triage sys",
            None,
        )
        .with_prompt_cache(PromptCachePolicy::Explicit),
    );
    boot.role.role.transport.keepalive_interval = Some(PING_AT);
    let host = Swansong::new();
    let runtime = started(boot, &host).await;
    (host, runtime, dir)
}

/// The triage request bodies and the pings, apart from everything else `upstream` was
/// sent.
fn triage_bodies(upstream: &ScriptedUpstream) -> Vec<Value> {
    upstream
        .requests()
        .iter()
        .map(Request::json)
        .filter(|b| b["model"] == TRIAGE_MODEL)
        .collect()
}

/// Whether a recorded body is a keepalive ping: the Messages ping is the only request
/// sent with `max_tokens: 0`.
fn is_ping(body: &Value) -> bool {
    body["max_tokens"] == 0
}

/// A session whose turn — a bash call, its triage, the closing prose — has run, returned
/// once the triage's verdict is logged.
async fn after_a_triage(
    upstream: &ScriptedUpstream,
) -> (Swansong, Arc<Session>, SessionRuntime, TempDir) {
    let (host, runtime, dir) = start_with_triage_model(upstream).await;
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    session
        .wait_until_with(upstream, "the triage's verdict", || {
            triage_bodies(upstream).iter().any(|b| !is_ping(b))
                && session
                    .snapshot()
                    .iter()
                    .any(|e| matches!(e.payload, Payload::Triage(_)))
        })
        .await;
    (host, session, runtime, dir)
}

/// Wait for the first triage ping and return it with the triage request that got the
/// verdict.
async fn the_ping_and_its_request(
    session: &Session,
    upstream: &ScriptedUpstream,
) -> (Value, Value) {
    session
        .wait_until_with(upstream, "the triage ping", || {
            triage_bodies(upstream).iter().any(is_ping)
        })
        .await;
    let sent = triage_bodies(upstream);
    let request = sent.iter().rfind(|b| !is_ping(b)).unwrap().clone();
    let ping = sent.into_iter().find(is_ping).unwrap();
    (request, ping)
}

/// Every envelope field the cache keys on is the request's own, and the ping's messages
/// are the request's up to its breakpoint (the end of the text view's history): the
/// current step after it is dropped.
fn assert_ping_is_the_request_through_its_breakpoint(request: &Value, ping: &Value) {
    for field in [
        "model",
        "system",
        "tools",
        "tool_choice",
        "thinking",
        "output_config",
    ] {
        assert_eq!(ping.get(field), request.get(field), "{field}");
    }
    let messages = ping["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "the history turn alone: {ping}");
    assert_eq!(messages[0], request["messages"][0]);
    let last = messages[0]["content"].as_array().unwrap().last().unwrap();
    assert_eq!(
        last["cache_control"]["type"], "ephemeral",
        "the history ends on its breakpoint"
    );
    assert!(
        request["messages"].as_array().unwrap().len() > 1,
        "the request carried the current step after it"
    );
}

/// A separate Messages triage lineage pings at the interval after its last triage
/// request, and the ping is that request up to its breakpoint, under its forced `retain`.
/// Once the turn's bash call has run the clock is paused (`upstream_hold.rs`) and the
/// lineage's clocks are set back to a minute short of the interval: the loop ticks past
/// it for five minutes and sends nothing, and the minute that makes it up brings the
/// ping at the next tick.
#[tokio::test]
async fn a_messages_triage_lineage_pings_its_last_request_at_the_interval() {
    const MINUTE: Duration = Duration::from_secs(60);
    let upstream = ScriptedUpstream::new(|n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        1 => Canned::stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)),
        2 => Canned::stream(end_turn()),
        _ => Canned::json(200, PING),
    });
    let (host, session, _runtime, _dir) = after_a_triage(&upstream).await;
    tokio::time::pause();
    keepalive::idle_for(&session, PING_AT - MINUTE);
    tokio::time::sleep(5 * MINUTE).await;
    assert!(
        !triage_bodies(&upstream).iter().any(is_ping),
        "no ping before the interval has passed"
    );
    keepalive::idle_for(&session, MINUTE);
    let (request, ping) = the_ping_and_its_request(&session, &upstream).await;
    host.shut_down().await;
    assert_eq!(request["tool_choice"]["name"], "retain");
    assert_ping_is_the_request_through_its_breakpoint(&request, &ping);
}

/// A model that refused the forced `tool_choice` is pinged with the unforced form it was
/// last sent: `auto`, as the fallback sent it. The fallback's instruction rides after the
/// breakpoint and is dropped with the current step. The interval is not the subject: once
/// the turn's bash call has run, the clock is paused (`upstream_hold.rs`) and the session
/// set back past the interval, so the first tick pings.
#[tokio::test]
async fn a_model_that_refused_forced_choice_is_pinged_unforced() {
    let upstream = ScriptedUpstream::new(|n| match n {
        0 => Canned::stream(tool_call("toolu_1", "bash", r#"{"command":"echo hi"}"#)),
        1 => Canned::json(400, FORCED_REFUSAL),
        2 => Canned::stream(tool_call("toolu_r", "retain", r#"{"keep":"all"}"#)),
        3 => Canned::stream(end_turn()),
        _ => Canned::json(200, PING),
    });
    let (host, session, _runtime, _dir) = after_a_triage(&upstream).await;
    tokio::time::pause();
    keepalive::idle_for(&session, PING_AT);
    let (request, ping) = the_ping_and_its_request(&session, &upstream).await;
    host.shut_down().await;
    assert_eq!(request["tool_choice"]["type"], "auto", "{request}");
    assert!(
        request["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .to_string()
            .contains("calling the `retain` tool"),
        "the request is the fallback's"
    );
    assert_ping_is_the_request_through_its_breakpoint(&request, &ping);
}
