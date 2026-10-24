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
    /// The credential itself expired, rather than being wrong. A provider that
    /// names the expiry (`token_expired`) is the one case classified apart from
    /// the other rejected credentials, which [`rejects_credential`] recognizes
    /// without a class of their own (docs/transport.md). Terminal if the
    /// re-read changes nothing.
    CredentialExpired,
}

/// Names for a credential that was valid and aged out, as distinct from one
/// the provider calls wrong. Either is worth one re-read of the credential's
/// source ([`rejects_credential`]); this one is also the class a retry loop can
/// read without parsing the body.
const EXPIRED: &[&str] = &[
    // The Codex backend, on a ChatGPT access token past its `exp`.
    "token_expired",
];

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
    // z.ai, whose codes are numeric strings: no balance and no Coding Plan
    // package, answered as a 429 whose `type` says `rate_limit_error`.
    "1113",
];

/// The [`TERMINAL`] names that say the credential itself was refused, as distinct
/// from the account behind it (quota, spend limits) or the request. A key the
/// operator revoked or replaced is named by one of these, and re-authorizing
/// stores a new value for the source to yield.
const CREDENTIAL: &[&str] = &[
    "invalid_api_key",
    "authentication_error",
    "permission_error",
];

/// Classify by the provider's `type` and `code`. Anything not named terminal is
/// transient — the current behavior, and Codex CLI's default for an unnamed
/// `response.failed` — so an unknown code keeps the bounded retry rather than
/// stalling a session on a name this table has not met. The name stays visible
/// in the record-only entry and the stall detail either way.
pub fn classify(kind: Option<&str>, code: Option<&str>) -> FailureClass {
    let named = |table: &[&str], name: Option<&str>| name.is_some_and(|n| table.contains(&n));
    // Expiry is checked first: it names a terminal condition too (the
    // credential was rejected), and the difference is that this one has a
    // remedy.
    if named(EXPIRED, kind) || named(EXPIRED, code) {
        FailureClass::CredentialExpired
    } else if named(TERMINAL, kind) || named(TERMINAL, code) {
        FailureClass::Terminal
    } else {
        FailureClass::Transient
    }
}

/// The `type` and `code` a JSON error body names, in either shape both dialects
/// use: at the top level (`{"type": …, "code": …}`) or nested under `error`. A
/// body that is not JSON, or names nothing, yields neither.
fn body_names(body: &str) -> (Option<String>, Option<String>) {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return (None, None);
    };
    let field = |name: &str| {
        value
            .get("error")
            .and_then(|e| e.get(name))
            .or_else(|| value.get(name))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    // The outer `type` of an envelope is `"error"`, which names nothing; the
    // nested one is the class.
    (field("type").filter(|k| k != "error"), field("code"))
}

/// Classify a JSON error body (see [`body_names`] for the shapes). A body that is
/// not JSON, or names nothing, is transient.
pub fn classify_body(body: &str) -> FailureClass {
    let (kind, code) = body_names(body);
    classify(kind.as_deref(), code.as_deref())
}

/// Whether the body of a 401 or 403 says the credential was refused, so a second
/// read of its source is worth one attempt (docs/transport.md): the provider named
/// an expiry or one of the [`CREDENTIAL`] names, or named nothing this table
/// knows (a gateway's HTML page, a code the table has not met). A name for
/// something else — a quota, a spend limit, a missing model — is not a refusal of
/// the credential, and re-reading it would change nothing.
pub fn rejects_credential(body: &str) -> bool {
    let (kind, code) = body_names(body);
    let (kind, code) = (kind.as_deref(), code.as_deref());
    let named = |name: Option<&str>| name.is_some_and(|n| CREDENTIAL.contains(&n));
    match classify(kind, code) {
        FailureClass::CredentialExpired | FailureClass::Transient => true,
        FailureClass::Terminal => named(kind) || named(code),
        FailureClass::Aborted => false,
    }
}

/// A request that failed to open a response stream (docs/transport.md): the
/// error [`Client::complete`](super::Client::complete) returns when no stream
/// opened, carrying its class so a retry loop can tell an outage from a refusal
/// without parsing the message.
#[derive(Debug)]
pub struct OpenError {
    message: String,
    class: FailureClass,
    status: Option<u16>,
}

impl OpenError {
    /// Classify an open failure by its status and the upstream's words. No status
    /// means no response arrived — a refused or timed-out connection — which is
    /// transient. The statuses the transport itself retries are transient unless
    /// the body names a terminal condition. Every other status is a rejection of
    /// the request, which a retry would repeat.
    pub(super) fn new(status: Option<u16>, message: String, detail: &str) -> Self {
        let class = match status {
            None => FailureClass::Transient,
            Some(408 | 429) | Some(500..=599) => match classify_body(detail) {
                // An expired credential on a retryable status: the transport re-reads
                // only on 401 and 403, and a resend with the same value cannot succeed.
                FailureClass::CredentialExpired => FailureClass::Terminal,
                class => class,
            },
            Some(_) => FailureClass::Terminal,
        };
        Self {
            message,
            class,
            status,
        }
    }

    /// The HTTP status the upstream answered with; `None` when no response arrived.
    pub fn status(&self) -> Option<u16> {
        self.status
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OpenError {}

/// How a retry loop should treat an `Err` from
/// [`Client::complete`](super::Client::complete): an [`OpenError`]'s own class,
/// and terminal for anything else — an encoding failure or a dialect mismatch is
/// a property of the request, and resending it changes nothing.
pub fn error_class(error: &super::Error) -> FailureClass {
    error
        .downcast_ref::<OpenError>()
        .map_or(FailureClass::Terminal, |open| open.class)
}

/// The HTTP status of a request the upstream refused, when it answered at all.
pub fn error_status(error: &super::Error) -> Option<u16> {
    error
        .downcast_ref::<OpenError>()
        .and_then(OpenError::status)
}

/// Whether the upstream refused something the request carries, so the same
/// request without an optional part might succeed: any 4xx except the four about
/// the caller rather than the content (401, 403, 408, 429). The fallbacks that
/// drop a part and retry (a forced `tool_choice`, image blocks — docs/transport.md)
/// read this rather than one status: OpenRouter answers an image sent to a
/// text-only model with a 404, a direct endpoint with a 400. The retry is the
/// diagnosis, so a 4xx with another cause costs one request and teaches nothing.
pub fn is_request_rejection(error: &super::Error) -> bool {
    error_status(error)
        .is_some_and(|s| (400..500).contains(&s) && ![401, 403, 408, 429].contains(&s))
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

    /// z.ai's no-balance 429 on its Messages endpoint, as answered 2026-10-24.
    /// The `type` names a rate limit, so the `code` is what makes it terminal.
    #[test]
    fn the_zai_insufficient_balance_body_is_terminal() {
        let body = r#"{"type":"error","error":{"type":"rate_limit_error","code":"1113","message":"[1113][Insufficient balance or no resource package. Please recharge.][202610250330231ec1a6df3236462f]"},"request_id":"202610250330231ec1a6df3236462f"}"#;
        assert_eq!(classify_body(body), FailureClass::Terminal);
        let e: crate::api::Error = OpenError::new(Some(429), "429".into(), body).into();
        assert_eq!(error_class(&e), FailureClass::Terminal);
    }

    #[test]
    fn a_messages_error_envelope_reads_the_nested_type() {
        let body = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(classify_body(body), FailureClass::Transient);
        let body = r#"{"type":"error","error":{"type":"permission_error","message":"no"}}"#;
        assert_eq!(classify_body(body), FailureClass::Terminal);
    }

    #[test]
    fn an_open_failure_without_a_response_is_transient() {
        let e: crate::api::Error =
            OpenError::new(None, "Conn took longer than 29s".into(), "").into();
        assert_eq!(error_class(&e), FailureClass::Transient);
    }

    #[test]
    fn an_open_failure_on_a_retryable_status_reads_the_body() {
        let overloaded = r#"{"type":"error","error":{"type":"overloaded_error"}}"#;
        let e: crate::api::Error = OpenError::new(Some(503), "503".into(), overloaded).into();
        assert_eq!(error_class(&e), FailureClass::Transient);
        let spent = r#"{"error":{"type":"usage_limit_reached"}}"#;
        let e: crate::api::Error = OpenError::new(Some(429), "429".into(), spent).into();
        assert_eq!(error_class(&e), FailureClass::Terminal);
    }

    #[test]
    fn an_open_failure_on_any_other_status_is_terminal() {
        let e: crate::api::Error = OpenError::new(Some(400), "400".into(), "bad").into();
        assert_eq!(error_class(&e), FailureClass::Terminal);
        let other: crate::api::Error = "dialect mismatch".into();
        assert_eq!(error_class(&other), FailureClass::Terminal);
    }

    #[test]
    fn a_request_rejection_is_a_4xx_not_about_the_caller() {
        let status =
            |s| -> crate::api::Error { OpenError::new(Some(s), s.to_string(), "x").into() };
        for s in [400, 404, 413, 415, 422] {
            assert!(is_request_rejection(&status(s)), "{s}");
        }
        for s in [401, 403, 408, 429, 500, 503] {
            assert!(!is_request_rejection(&status(s)), "{s}");
        }
        assert!(!is_request_rejection(&"dialect mismatch".into()));
    }

    #[test]
    fn a_non_json_body_is_transient() {
        assert_eq!(classify_body("<html>502</html>"), FailureClass::Transient);
        assert_eq!(classify_body(""), FailureClass::Transient);
    }
}
