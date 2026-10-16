//! Tests for `keepalive.rs` — the two pure predicates the ping is built on.

use super::*;
use crate::api::KeepaliveError;
use crate::api::{RequestUsage, TokenPrices};
use crate::daemon::keepalive::{
    CacheLineage, KeepaliveSkipReason, KeepaliveSpec, PingCost, discard_is_affordable,
    keepalive_interval, lop_trailing_model_turns, prefix_tokens, skip_reason,
};
use crate::log::{AssistantOutput, Mark, MessagesOutputBlock, Timestamp};
use crate::project::{Role, WireTurn};

fn message(role: Role, text: &str) -> WireTurn {
    match role {
        Role::User => WireTurn::text(text),
        Role::Assistant => {
            WireTurn::Assistant(AssistantOutput::Messages(vec![MessagesOutputBlock::Text(
                text.into(),
            )]))
        }
    }
}

fn roles(messages: &[WireTurn]) -> Vec<Role> {
    messages
        .iter()
        .map(|m| match m {
            WireTurn::User(_) => Role::User,
            WireTurn::Assistant(_) => Role::Assistant,
            WireTurn::Breakpoint => unreachable!("no breakpoint in these fixtures"),
        })
        .collect()
}

/// A log parked at a gate (or settled) ends with the model's own output, which some
/// providers reject as input. Everything up to the last user-role message survives.
#[test]
fn lopping_removes_trailing_model_turns() {
    let mut messages = vec![
        message(Role::User, "one"),
        message(Role::Assistant, "two"),
        message(Role::User, "tool result"),
        message(Role::Assistant, "closing prose"),
    ];
    lop_trailing_model_turns(&mut messages);
    assert_eq!(
        roles(&messages),
        vec![Role::User, Role::Assistant, Role::User]
    );

    // Consecutive model turns all go, not just the last one.
    let mut messages = vec![
        message(Role::User, "one"),
        message(Role::Assistant, "two"),
        message(Role::Assistant, "three"),
    ];
    lop_trailing_model_turns(&mut messages);
    assert_eq!(roles(&messages), vec![Role::User]);
}

/// A tail that is already a user message (a tool result mid-turn) is left alone.
#[test]
fn lopping_leaves_a_user_tail_untouched() {
    let original = vec![
        message(Role::User, "one"),
        message(Role::Assistant, "two"),
        message(Role::User, "tool result"),
    ];
    let mut messages = original.clone();
    lop_trailing_model_turns(&mut messages);
    assert_eq!(messages, original);
}

/// The two degenerate logs: nothing but model turns, and nothing at all. Both leave
/// no messages, which is the loop's signal to skip the ping rather than send an
/// empty `messages` array.
#[test]
fn lopping_can_empty_the_messages() {
    let mut all_model = vec![
        message(Role::Assistant, "one"),
        message(Role::Assistant, "two"),
    ];
    lop_trailing_model_turns(&mut all_model);
    assert!(all_model.is_empty());

    let mut empty: Vec<WireTurn> = vec![];
    lop_trailing_model_turns(&mut empty);
    assert!(empty.is_empty());
}

/// The ratio boundary: discarded output is tolerated strictly below one token per
/// hundred prefix tokens, and a ping that generated nothing always passes.
#[test]
fn discard_ratio_boundary() {
    assert!(discard_is_affordable(0, 0));
    assert!(discard_is_affordable(0, 500_000));
    // 100 × 1000 == 100_000 is not strictly less than the prefix.
    assert!(!discard_is_affordable(1_000, 100_000));
    assert!(discard_is_affordable(1_000, 100_001));
    assert!(!discard_is_affordable(1_000, 99_999));
    // The self-scaling ends: a tiny prefix tolerates no discarded output at all.
    assert!(!discard_is_affordable(1, 50));
    // A large prefix absorbs a mandatory-reasoning endpoint's few thousand tokens.
    assert!(discard_is_affordable(4_000, 500_000));
}

/// Prefix size is whatever the endpoint billed as prompt, however it split the
/// prefix across fresh input, cache read, and cache write.
#[test]
fn prefix_tokens_sums_every_prompt_field() {
    let usage = Usage {
        input_tokens: 3,
        output_tokens: 900,
        cache_creation_input_tokens: 20,
        cache_read_input_tokens: 100,
    };
    assert_eq!(prefix_tokens(&usage), 123);
}

/// The retry decision reads the status structurally: a rejected request is rejected
/// identically next tick, while the moment-bound 4xx and everything else retry.
#[test]
fn permanent_classification() {
    let status = |status| KeepaliveError::Status {
        status: Some(status),
        body: "…".into(),
    };
    assert!(status(400).is_permanent());
    assert!(status(404).is_permanent());
    assert!(status(422).is_permanent());
    assert!(status(401).is_permanent());
    assert!(!status(408).is_permanent());
    assert!(!status(429).is_permanent());
    assert!(!status(500).is_permanent());
    assert!(!status(503).is_permanent());
    assert!(
        !KeepaliveError::Status {
            status: None,
            body: "…".into()
        }
        .is_permanent()
    );
    assert!(!KeepaliveError::other("connection reset").is_permanent());
}

/// Believed cold from the last main-prefix request plus the cache lifetime, and not
/// before it; a session that has sent nothing has nothing to lose.
#[test]
fn believed_cold_from_last_touch_plus_lifetime() {
    use crate::daemon::keepalive::believed_cold_since;
    use crate::log::Timestamp;
    let ttl = Duration::from_secs(300);
    let touch = Timestamp(1_000_000);
    assert_eq!(believed_cold_since(None, Timestamp(9_999_999), ttl), None);
    assert_eq!(
        believed_cold_since(Some(touch), Timestamp(1_300_000), ttl),
        None
    );
    assert_eq!(
        believed_cold_since(Some(touch), Timestamp(1_300_001), ttl),
        Some(Timestamp(1_300_000))
    );
}

/// z-ai/glm-5.3-flash on the Z.AI endpoint: $0.15/M prompt, $0.50/M completion,
/// $0.03/M cache read.
fn glm() -> TokenPrices {
    TokenPrices {
        prompt: 0.15e-6,
        completion: 0.50e-6,
        cache_read: 0.03e-6,
    }
}

fn ping(output_tokens: u64, completion_usd: Option<f64>) -> PingCost {
    PingCost {
        output_tokens,
        completion_usd,
    }
}

/// The priced rule (docs/transport.md): a 10k-token ping (~$0.005) against a 200k
/// prefix, whose cold-read premium is 200k × $0.12/M = $0.024, is affordable; the
/// fixed token ratio would have refused it.
#[test]
fn priced_rule_accepts_the_glm_example() {
    let prices = glm();
    let cost = 10_000.0 * prices.completion;
    assert!(skip_reason(ping(10_000, Some(cost)), 200_000, Some(&prices)).is_none());
    assert!(!discard_is_affordable(10_000, 200_000));
}

/// A ping whose completion cost exceeds half the premium is refused, with the
/// numbers that decided it.
#[test]
fn priced_rule_refuses_a_ping_costing_over_half_the_premium() {
    let prices = glm();
    // Premium at 200k is $0.024; half is $0.012.
    assert!(skip_reason(ping(23_000, Some(0.0115)), 200_000, Some(&prices)).is_none());
    let why = skip_reason(ping(25_000, Some(0.0125)), 200_000, Some(&prices)).unwrap();
    let KeepaliveSkipReason::Discard {
        output_tokens,
        completion_usd,
        prefix_tokens,
        premium_usd,
    } = why
    else {
        panic!("a discard stance")
    };
    assert_eq!((output_tokens, prefix_tokens), (25_000, 200_000));
    assert_eq!(completion_usd, Some(0.0125));
    assert!((premium_usd.unwrap() - 0.024).abs() < 1e-9);
}

/// Without prices the token-ratio rule decides, and a ping that generated nothing
/// passes either rule.
#[test]
fn unpriced_sessions_fall_back_to_the_token_ratio() {
    assert!(skip_reason(ping(1_000, None), 100_001, None).is_none());
    assert!(skip_reason(ping(1_000, None), 100_000, None).is_some());
    assert!(skip_reason(ping(0, None), 10, None).is_none());
    assert!(skip_reason(ping(0, Some(0.0)), 10, Some(&glm())).is_none());
}

/// The completion cost is the receipt's inline figure when it has one, else the
/// session's completion price over the discarded tokens.
#[test]
fn completion_cost_prefers_the_receipt() {
    use crate::api::{InlineCost, Receipt};
    use crate::daemon::keepalive::completion_cost;
    let usage = Usage {
        output_tokens: 1_000,
        ..Usage::default()
    };
    let bare = RequestUsage::new(usage.clone());
    assert_eq!(completion_cost(&bare, None), None);
    assert!((completion_cost(&bare, Some(&glm())).unwrap() - 0.0005).abs() < 1e-12);
    let billed = RequestUsage::new(usage).with_receipt(Receipt {
        served_by: None,
        cost: Some(InlineCost {
            total_usd: 0.02,
            prompt_usd: None,
            completion_usd: Some(0.003),
        }),
    });
    assert_eq!(completion_cost(&billed, Some(&glm())), Some(0.003));
}

/// A canned upstream for the loop tests: answers every request with the status and
/// body `reply` picks by arrival order, closing each connection, and counts them.
fn serve(
    reply: impl Fn(usize) -> (u16, String) + Send + 'static,
) -> (std::net::SocketAddr, Arc<std::sync::atomic::AtomicUsize>) {
    let (addr, count, _) = serve_recording(reply);
    (addr, count)
}

/// [`serve`], also keeping each request's body in arrival order.
fn serve_recording(
    reply: impl Fn(usize) -> (u16, String) + Send + 'static,
) -> (
    std::net::SocketAddr,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::Mutex<Vec<String>>>,
) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(mut sock) = sock else { return };
            let mut request = String::new();
            let mut buf = [0u8; 16384];
            loop {
                let read = sock.read(&mut buf).unwrap_or(0);
                request.push_str(&String::from_utf8_lossy(&buf[..read]));
                let complete = request.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                    head.to_ascii_lowercase()
                        .split("\r\n")
                        .find_map(|line| line.strip_prefix("content-length: ").map(str::to_string))
                        .and_then(|n| n.trim().parse::<usize>().ok())
                        .is_some_and(|n| body.len() >= n)
                });
                if read == 0 || complete {
                    break;
                }
            }
            // Recorded before the count moves, so a waiter that sees the count sees the body.
            recorded.lock().unwrap().push(
                request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body.to_string())
                    .unwrap_or_default(),
            );
            let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (status, body) = reply(n);
            let _ = sock.write_all(
                format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                     connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = sock.flush();
        }
    });
    (addr, count, bodies)
}

/// The main lineage's loop as the runtime spawns it, with the auto envelope.
fn main_loop(
    client: Arc<crate::api::Client>,
    shared: Arc<Session>,
    tools: Vec<Value>,
    ping_at: Duration,
    prices: Option<TokenPrices>,
) -> impl std::future::Future<Output = ()> {
    super::keepalive::keepalive_loop(
        KeepaliveSpec {
            lineage: CacheLineage::Main,
            client,
            tools,
            choice: ToolChoice::auto(),
            ping_at,
            prices,
        },
        shared,
    )
}

/// A session with one user message and one recorded turn of `prefix` prompt tokens,
/// and a client pointed at `addr`.
fn pinged_session(
    addr: std::net::SocketAddr,
    prefix: u64,
) -> (Arc<Session>, Arc<crate::api::Client>) {
    use crate::api::{Client, Endpoint, Provider};
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared
        .append(1, Payload::User(vec![ContentBlock::Text("hi".into())]))
        .unwrap();
    record_turn(&shared, prefix);
    let endpoint = Endpoint {
        provider: Provider::Anthropic,
        url: Some(format!("http://{addr}/v1/messages")),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    (
        shared,
        Arc::new(Client::new(endpoint, "claude-opus-5", "sys", None)),
    )
}

fn record_turn(shared: &Session, prefix: u64) {
    shared.record_request(
        RequestOrigin::Turn,
        RequestUsage::new(Usage {
            input_tokens: prefix,
            ..Usage::default()
        }),
    );
}

fn skipped(shared: &Session) -> Value {
    let state: Value = serde_json::from_str(&shared.state_json()).unwrap();
    state["usage"]["keepalive_skipped"].clone()
}

/// An expensive first ping at a small prefix makes the next tick skip, with the stance
/// in `/state`; the same estimate against a prefix that has grown pings again and
/// clears it. Real one-second ticks (the loop's floor).
#[tokio::test]
async fn an_expensive_ping_skips_until_the_prefix_repays_it() {
    let ping_body = r#"{"id":"m","model":"m","usage":{"input_tokens":5000,"output_tokens":10000}}"#;
    let (addr, count) = serve(move |_| (200, ping_body.to_string()));
    let (shared, client) = pinged_session(addr, 5_000);
    let task = tokio::spawn(main_loop(
        client,
        Arc::clone(&shared),
        Vec::new(),
        Duration::from_millis(1),
        None,
    ));
    let sent = |n| count.load(std::sync::atomic::Ordering::SeqCst) == n;
    let wait_for = async |what: &dyn Fn() -> bool| {
        for _ in 0..80 {
            if what() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out");
    };
    // The first ping goes out unjudged; the following ticks skip.
    wait_for(&|| sent(1) && skipped(&shared).is_object()).await;
    let stance = skipped(&shared);
    assert_eq!(stance["reason"], "discard");
    assert_eq!(stance["output_tokens"], 10_000);
    assert_eq!(stance["prefix_tokens"], 5_000);
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(sent(1), "skipped ticks send nothing");

    // The prefix grows past break-even: 10k discarded tokens need over 1M prefix.
    record_turn(&shared, 2_000_000);
    wait_for(&|| !sent(1)).await;
    wait_for(&|| skipped(&shared).is_null()).await;
    task.abort();
}

/// A permanent rejection still ends the loop, and the state says why.
#[tokio::test]
async fn a_permanent_rejection_ends_the_loop() {
    let (addr, count) = serve(|_| (400, r#"{"error":"bad"}"#.to_string()));
    let (shared, client) = pinged_session(addr, 5_000);
    let task = tokio::spawn(main_loop(
        client,
        Arc::clone(&shared),
        Vec::new(),
        Duration::from_millis(1),
        None,
    ));
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the loop returns")
        .unwrap();
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    let stance = skipped(&shared);
    assert_eq!(stance["reason"], "rejected");
    assert!(stance["detail"].as_str().unwrap().contains("400"));
}

/// A plan's usage limit (a 429 whose body names it) pauses the pings instead of
/// retrying them every tick, with the reset time in the stance; the next real turn
/// resumes them.
#[tokio::test]
async fn an_exhausted_limit_pauses_until_the_next_turn() {
    let limit = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_at":1791623333,"resets_in_seconds":13597}}"#;
    let ok = r#"{"id":"m","model":"m","usage":{"input_tokens":5000,"output_tokens":1}}"#;
    let (addr, count) = serve(move |n| match n {
        0 => (429, limit.to_string()),
        _ => (200, ok.to_string()),
    });
    let (shared, client) = pinged_session(addr, 5_000);
    let task = tokio::spawn(main_loop(
        client,
        Arc::clone(&shared),
        Vec::new(),
        Duration::from_millis(1),
        None,
    ));
    let sent = |n| count.load(std::sync::atomic::Ordering::SeqCst) == n;
    let wait_for = async |what: &dyn Fn() -> bool| {
        for _ in 0..80 {
            if what() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out");
    };
    wait_for(&|| sent(1) && skipped(&shared).is_object()).await;
    let stance = skipped(&shared);
    assert_eq!(stance["reason"], "exhausted");
    assert_eq!(stance["resets_at"], 1_791_623_333_000u64);
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(sent(1), "paused ticks send nothing");

    record_turn(&shared, 5_000);
    wait_for(&|| !sent(1)).await;
    wait_for(&|| skipped(&shared).is_null()).await;
    task.abort();
}

/// A `usage` with `prefix` prompt tokens, for recording a request by hand.
fn prompt_usage(prefix: u64) -> RequestUsage {
    RequestUsage::new(Usage {
        input_tokens: prefix,
        ..Usage::default()
    })
}

/// The Messages endpoint a canned upstream at `addr` answers on.
fn messages_endpoint(addr: std::net::SocketAddr) -> crate::api::Endpoint {
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

/// Poll `what` until it holds, failing after ten seconds.
async fn wait_until(what: impl Fn() -> bool) {
    for _ in 0..100 {
        if what() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out");
}

/// The triage lineage's ping replays the primed turns under the envelope its loop was
/// given (docs/transport.md): the primer's forced `retain` and the peek's tools, over the
/// turns the primer sent and nothing else.
#[tokio::test]
async fn a_triage_ping_replays_the_primed_turns_under_the_primers_envelope() {
    let ok = r#"{"id":"m","model":"m","usage":{"input_tokens":5000,"output_tokens":1}}"#;
    let (addr, count, bodies) = serve_recording(move |_| (200, ok.to_string()));
    let (shared, _main) = pinged_session(addr, 5_000);
    let turns = vec![WireTurn::text("the turn's own input")];
    shared.set_triage_primed(&turns, Timestamp::now(), &prompt_usage(5_000));
    shared.record_request(RequestOrigin::TriagePrimer, prompt_usage(5_000));
    let spec = KeepaliveSpec {
        lineage: CacheLineage::Triage,
        client: Arc::new(crate::api::Client::new(
            messages_endpoint(addr),
            "claude-haiku-5",
            "sys",
            None,
        )),
        tools: vec![
            serde_json::json!({"name": "retain"}),
            serde_json::json!({"name": "triage"}),
        ],
        choice: ToolChoice::force("retain"),
        ping_at: Duration::from_millis(1),
        prices: None,
    };
    let task = tokio::spawn(super::keepalive::keepalive_loop(spec, Arc::clone(&shared)));
    wait_until(|| count.load(std::sync::atomic::Ordering::SeqCst) == 1).await;
    task.abort();

    let body: Value = serde_json::from_str(&bodies.lock().unwrap()[0]).unwrap();
    assert_eq!(
        body["tool_choice"],
        serde_json::json!({"type": "tool", "name": "retain", "disable_parallel_tool_use": true}),
        "the forced choice the peek sends, parallel calls off"
    );
    let tools: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["retain", "triage"]);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "the primed turns, and nothing appended");
    assert!(messages[0].to_string().contains("the turn's own input"));
}

/// The triage keepalive is armed only where its primer can plant: an own triage model on
/// an implicit Responses lineage. A shared triage has no lineage of its own, and a
/// Messages-dialect triage model is not primed. The configured interval applies to both
/// lineages, and zero turns the triage one off.
#[test]
fn the_triage_keepalive_arms_only_where_a_primer_plants() {
    use crate::api::{Client, Endpoint, PromptCachePolicy, Provider};
    use crate::triage::ForkPeek;
    assert!(KeepaliveSpec::triage(&ForkPeek::shared(), None, None).is_none());
    let messages = ForkPeek::own(Client::new(
        messages_endpoint("127.0.0.1:9".parse().unwrap()),
        "claude-haiku-5",
        "sys",
        None,
    ));
    assert!(KeepaliveSpec::triage(&messages, None, None).is_none());

    let codex = Endpoint {
        provider: Provider::OpenAICodex,
        url: Some("http://127.0.0.1:9/backend-api/codex/responses".into()),
        auth_header: "authorization".into(),
        auth_value: "Bearer k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    let peek = ForkPeek::own(
        Client::new(codex, "gpt-6.1-sol", "sys", None)
            .with_prompt_cache(PromptCachePolicy::Implicit),
    );
    let armed = KeepaliveSpec::triage(&peek, None, None)
        .expect("an implicit Responses lineage plants a primer");
    assert_eq!(armed.lineage, CacheLineage::Triage);
    assert_eq!(
        armed.ping_at,
        keepalive_interval(Duration::from_secs(10 * 60))
    );
    let names: Vec<Value> = armed.tools.iter().map(|t| t["name"].clone()).collect();
    assert_eq!(
        names,
        [serde_json::json!("retain"), serde_json::json!("triage")]
    );
    assert_eq!(armed.choice.forced(), Some("retain"));
    let off = KeepaliveSpec::triage(&peek, Some(Duration::ZERO), None).unwrap();
    assert!(off.ping_at.is_zero());
}

/// A fold moves the prefix the primer read. The stored turns stop being replayed, and the
/// triage clock's fold filter empties the ping's prefix, as it does the main loop's.
#[tokio::test]
async fn a_fold_drops_the_primed_turns() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_triage_primed(
        &[WireTurn::text("input")],
        Timestamp::now(),
        &prompt_usage(5_000),
    );
    shared.record_request(RequestOrigin::TriagePrimer, prompt_usage(5_000));
    assert!(shared.ping_turns(CacheLineage::Triage).is_some());
    tokio::time::sleep(Duration::from_millis(5)).await;
    shared
        .append(1, Payload::Mark(Mark::boundary(0, "account")))
        .unwrap();
    assert_eq!(shared.ping_turns(CacheLineage::Triage), None);
    assert_eq!(shared.ping_prefix_tokens(CacheLineage::Triage), None);
    assert_eq!(shared.keepalive_clocks(CacheLineage::Triage).0, None);
}

/// Idle triage runs on the agent's client, so it leaves the triage clock alone. A triage
/// ping stamps that clock and not the real-turn clock, so it cannot reset the give-up cap.
#[tokio::test]
async fn idle_triage_is_no_touch_and_a_triage_ping_keeps_the_cap() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
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
