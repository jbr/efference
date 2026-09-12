//! Whether an upstream failure is worth retrying (docs/transport.md).
//!
//! Both dialects deliver some failures inside a successful response — a stream
//! that opened 200 and then carried an `error` event — and some as a non-2xx
//! open. Either way the provider names the failure with a type and a code, and
//! that name is what separates a condition a retry can outlast (an overloaded
//! server, a per-minute rate limit) from one it cannot (an exhausted plan, a
//! rejected request, a bad credential). The retry loops read the class, not the
//! prose: a terminal failure stalls on the first attempt, a transient one takes
//! the bounded abort schedule.

use serde_json::Value;

/// How the retry loops should treat an upstream failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureClass {
    /// A retry may outlast it: the abort schedule applies.
    Transient,
    /// No retry can change it: stall at once, with the provider's words.
    Terminal,
    /// The operator ended the read (`POST /loop/interrupt` mid-generation): no
    /// retry, no stall — the turn ends and the loop returns to idle. Counts
    /// against nothing.
    Aborted,
}

/// Names a provider attaches to failures that a retry cannot change: quota and
/// plan exhaustion, rejected requests, credentials, policy refusals, and the
/// context window. Matched against both the `type` and the `code` field, since
/// the two dialects (and the Codex backend) put the operative name in either.
const TERMINAL: &[&str] = &[
    // Anthropic Messages error types.
    "invalid_request_error",
    "authentication_error",
    "permission_error",
    "not_found_error",
    "billing_error",
    // OpenAI Responses and the Codex backend.
    "insufficient_quota",
    "credit_balance_exhausted",
    "organization_spend_limit_exceeded",
    "project_spend_limit_exceeded",
    "organization_usage_limit_exceeded",
    "usage_not_included",
    "usage_limit_reached",
    "invalid_api_key",
    "invalid_prompt",
    "model_not_found",
    "context_length_exceeded",
    "cyber_policy",
    "misalignment_policy_violation",
];

/// Classify by the provider's `type` and `code`. Anything not named terminal is
/// transient — the current behavior, and Codex CLI's default for an unnamed
/// `response.failed` — so an unknown code keeps the bounded retry rather than
/// stalling a session on a name this table has not met. The name stays visible
/// in the record-only entry and the stall detail either way.
pub fn classify(kind: Option<&str>, code: Option<&str>) -> FailureClass {
    let terminal = |name: Option<&str>| name.is_some_and(|n| TERMINAL.contains(&n));
    if terminal(kind) || terminal(code) {
        FailureClass::Terminal
    } else {
        FailureClass::Transient
    }
}

/// Classify a JSON error body, in either shape both dialects use: the name at
/// the top level (`{"type": …, "code": …}`) or nested under `error`. A body
/// that is not JSON, or names nothing, is transient.
pub fn classify_body(body: &str) -> FailureClass {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return FailureClass::Transient;
    };
    let field = |name: &str| {
        value
            .get("error")
            .and_then(|e| e.get(name))
            .or_else(|| value.get(name))
            .and_then(Value::as_str)
    };
    // The outer `type` of an envelope is `"error"`, which names nothing; the
    // nested one is the class.
    let kind = field("type").filter(|k| *k != "error");
    classify(kind, field("code"))
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn quota_and_credential_names_are_terminal_in_either_field() {
        assert_eq!(
            classify(Some("insufficient_quota"), Some("credit_balance_exhausted")),
            FailureClass::Terminal
        );
        assert_eq!(
            classify(None, Some("usage_limit_reached")),
            FailureClass::Terminal
        );
        assert_eq!(
            classify(Some("authentication_error"), None),
            FailureClass::Terminal
        );
        assert_eq!(
            classify(Some("invalid_request_error"), None),
            FailureClass::Terminal
        );
    }

    #[test]
    fn load_and_rate_names_and_unknown_names_are_transient() {
        assert_eq!(
            classify(Some("overloaded_error"), None),
            FailureClass::Transient
        );
        assert_eq!(
            classify(Some("rate_limit_error"), None),
            FailureClass::Transient
        );
        assert_eq!(
            classify(None, Some("server_error")),
            FailureClass::Transient
        );
        assert_eq!(classify(None, Some("slow_down")), FailureClass::Transient);
        assert_eq!(
            classify(Some("never_seen_before"), None),
            FailureClass::Transient
        );
        assert_eq!(classify(None, None), FailureClass::Transient);
    }

    /// The Codex backend's exhausted-plan 429, as recorded in session 1788726248597.
    #[test]
    fn the_codex_usage_limit_body_is_terminal() {
        let body = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":1788744444,"eligible_promo":null,"resets_in_seconds":15805}}"#;
        assert_eq!(classify_body(body), FailureClass::Terminal);
    }

    #[test]
    fn a_messages_error_envelope_reads_the_nested_type() {
        let body = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(classify_body(body), FailureClass::Transient);
        let body = r#"{"type":"error","error":{"type":"permission_error","message":"no"}}"#;
        assert_eq!(classify_body(body), FailureClass::Terminal);
    }

    #[test]
    fn a_non_json_body_is_transient() {
        assert_eq!(classify_body("<html>502</html>"), FailureClass::Transient);
        assert_eq!(classify_body(""), FailureClass::Transient);
    }
}
