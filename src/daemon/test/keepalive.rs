//! Tests for `keepalive.rs` — the two pure predicates the ping is built on.

use super::*;
use crate::api::KeepaliveError;
use crate::daemon::keepalive::{discard_is_affordable, lop_trailing_model_turns, prefix_tokens};
use crate::project::{ApiBlock, ApiMessage};

fn message(role: Role, text: &str) -> ApiMessage {
    ApiMessage {
        role,
        content: vec![ApiBlock::Text {
            text: text.into(),
            cache_control: None,
        }],
    }
}

fn roles(messages: &[ApiMessage]) -> Vec<Role> {
    messages.iter().map(|m| m.role).collect()
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

    let mut empty: Vec<ApiMessage> = vec![];
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
