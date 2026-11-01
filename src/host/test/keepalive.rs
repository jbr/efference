//! Tests for `keepalive.rs` — the two pure predicates the ping is built on.

use super::*;
use crate::inference::api::KeepaliveError;
use crate::inference::api::{RequestUsage, TokenPrices};
use crate::inference::testing::upstream::{Canned, ScriptedUpstream};
use crate::protocol::state::KeepaliveSkipReason;
use crate::record::log::{AssistantOutput, MessagesOutputBlock};
use crate::record::project::{Role, WireTurn};
use crate::session::keepalive::{
    CacheLineage, KeepaliveSpec, PingCost, discard_is_affordable, lop_trailing_model_turns,
    prefix_tokens, skip_reason,
};

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
    use crate::record::log::Timestamp;
    use crate::session::keepalive::believed_cold_since;
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
    PingCost::new(output_tokens, completion_usd)
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
    use crate::inference::api::{InlineCost, Receipt};
    use crate::session::keepalive::completion_cost;
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

/// The main lineage's loop as the runtime spawns it, with the auto envelope.
fn main_loop(
    client: Arc<Client>,
    shared: Arc<Session>,
    tools: Vec<Value>,
    ping_at: Duration,
    prices: Option<TokenPrices>,
) -> impl Future<Output = ()> {
    keepalive_loop(
        KeepaliveSpec {
            lineage: CacheLineage::Main,
            client,
            tools,
            ping_at,
            prices,
        },
        shared,
    )
}

/// A session with one user message and one recorded turn of `prefix` prompt tokens,
/// and a client pointed at `upstream`.
pub(super) fn pinged_session(
    upstream: &ScriptedUpstream,
    prefix: u64,
) -> (Arc<Session>, Arc<Client>) {
    use crate::inference::api::Client;
    use crate::record::log::Provider;
    let (shared, _rx) = Session::in_memory();
    shared.append(1, Payload::user("hi")).unwrap();
    record_turn(&shared, prefix);
    let endpoint = upstream.endpoint(Provider::Anthropic, "/v1/messages");
    (
        shared,
        Arc::new(Client::new(endpoint, "claude-opus-5", "sys")),
    )
}

pub(super) fn record_turn(shared: &Session, prefix: u64) {
    shared.record_request(
        RequestOrigin::Turn,
        RequestUsage::new(Usage {
            input_tokens: prefix,
            ..Usage::default()
        }),
    );
}

/// The session as it stands after sitting idle for `idle`: its touch, real-turn and
/// triage clocks set back by that much. The keepalive reads them off `std::time::Instant`,
/// which a paused clock does not move, so a test that needs time to have passed on them
/// sets them back instead of waiting.
pub(super) fn idle_for(shared: &Session, idle: Duration) {
    let back = |at: &mut Option<Instant>| {
        *at = at.map(|at| at.checked_sub(idle).expect("a clock that can be set back"));
    };
    let mut metrics = shared.metrics_for_test().lock().unwrap();
    back(&mut metrics.last_request_at);
    back(&mut metrics.last_real_turn_at);
    back(&mut metrics.last_triage_at);
}

fn skipped(shared: &Session) -> Value {
    let state: Value = serde_json::from_str(&shared.state_json()).unwrap();
    state["usage"]["keepalive_skipped"].clone()
}

/// An expensive first ping at a small prefix makes the next tick skip, with the stance
/// in `/state`; the same estimate against a prefix that has grown pings again and
/// clears it. The ticks are the loop's one-second floor, taken on a paused clock; the
/// ping is due at once (`Duration::ZERO`), since the interval is read off
/// `std::time::Instant` and a paused clock does not move it.
#[tokio::test(start_paused = true)]
async fn an_expensive_ping_skips_until_the_prefix_repays_it() {
    let ping_body = r#"{"id":"m","model":"m","usage":{"input_tokens":5000,"output_tokens":10000}}"#;
    let upstream = ScriptedUpstream::new(move |_| Canned::json(200, ping_body));
    let (shared, client) = pinged_session(&upstream, 5_000);
    let task = tokio::spawn(main_loop(
        client,
        Arc::clone(&shared),
        Vec::new(),
        Duration::ZERO,
        None,
    ));
    let sent = |n| upstream.count() == n;
    // The first ping goes out unjudged; the following ticks skip.
    shared
        .wait_until_with(
            &upstream,
            "the first ping went out and the next tick skipped",
            || sent(1) && skipped(&shared).is_object(),
        )
        .await;
    let stance = skipped(&shared);
    assert_eq!(stance["reason"], "discard");
    assert_eq!(stance["output_tokens"], 10_000);
    assert_eq!(stance["prefix_tokens"], 5_000);
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(sent(1), "skipped ticks send nothing");

    // The prefix grows past break-even: 10k discarded tokens need over 1M prefix.
    record_turn(&shared, 2_000_000);
    shared
        .wait_until_with(&upstream, "the next ping went out", || !sent(1))
        .await;
    shared
        .wait_until("the skip stance cleared", || skipped(&shared).is_null())
        .await;
    task.abort();
}

/// A permanent rejection still ends the loop, and the state says why.
#[tokio::test(start_paused = true)]
async fn a_permanent_rejection_ends_the_loop() {
    let upstream = ScriptedUpstream::new(|_| Canned::json(400, r#"{"error":"bad"}"#));
    let (shared, client) = pinged_session(&upstream, 5_000);
    let task = tokio::spawn(main_loop(
        client,
        Arc::clone(&shared),
        Vec::new(),
        Duration::ZERO,
        None,
    ));
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the loop returns")
        .unwrap();
    assert_eq!(upstream.count(), 1);
    let stance = skipped(&shared);
    assert_eq!(stance["reason"], "rejected");
    assert!(stance["detail"].as_str().unwrap().contains("400"));
}

/// A plan's usage limit (a 429 whose body names it) pauses the pings instead of
/// retrying them every tick, with the reset time in the stance; the next real turn
/// resumes them. The pause ends when the real-turn clock reads below what it read at the
/// limit, so the session has been idle for ten minutes first: on `std::time::Instant`
/// readings of microseconds the new turn would not reliably read below them.
#[tokio::test(start_paused = true)]
async fn an_exhausted_limit_pauses_until_the_next_turn() {
    let limit = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_at":1791623333,"resets_in_seconds":13597}}"#;
    let ok = r#"{"id":"m","model":"m","usage":{"input_tokens":5000,"output_tokens":1}}"#;
    let upstream = ScriptedUpstream::new(move |n| match n {
        0 => Canned::json(429, limit),
        _ => Canned::json(200, ok),
    });
    let (shared, client) = pinged_session(&upstream, 5_000);
    idle_for(&shared, Duration::from_secs(10 * 60));
    let task = tokio::spawn(main_loop(
        client,
        Arc::clone(&shared),
        Vec::new(),
        Duration::ZERO,
        None,
    ));
    let sent = |n| upstream.count() == n;
    shared
        .wait_until_with(
            &upstream,
            "the first ping went out and the next tick skipped",
            || sent(1) && skipped(&shared).is_object(),
        )
        .await;
    let stance = skipped(&shared);
    assert_eq!(stance["reason"], "exhausted");
    assert_eq!(stance["resets_at"], 1_791_623_333_000u64);
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(sent(1), "paused ticks send nothing");

    record_turn(&shared, 5_000);
    shared
        .wait_until_with(&upstream, "the next ping went out", || !sent(1))
        .await;
    shared
        .wait_until("the skip stance cleared", || skipped(&shared).is_null())
        .await;
    task.abort();
}
