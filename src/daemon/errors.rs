//! The typed error envelope (docs/control-api.md).
//!
//! Every non-2xx body is one shape — `{code, message, detail?}` — so a client parses
//! errors once rather than pattern-matching human strings per route. `code` is the
//! machine-readable half and is a Rust enum, so adding a variant is visible at compile
//! time rather than appearing on the wire as a novel string; `message` stays the human
//! half and is free to change without breaking a client.
//!
//! The code set is deliberately small and derived from the errors the router already
//! returns, plus the one the gate's precondition needs ([`ErrorCode::GateMoved`]).
//! Resist growing it per-route: a code earns its place when a client would *act*
//! differently on it, not when the prose differs. (A `cursor_out_of_range` code was
//! reserved here once; it left when `?since` past the head was settled as a wait, not
//! an error — under wait-semantics no client acts on it, because it is not one.)

use super::*;

/// The machine-readable half of an error response. Each variant has one canonical HTTP
/// status ([`ErrorCode::status`]), so a handler picks the code and the status follows.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorCode {
    /// Malformed body, unparseable path parameter, out-of-range value. 400.
    BadRequest,
    /// A well-formed decision the daemon doesn't implement (an unknown approval
    /// decision, a proposal decision that is neither commit nor cancel). Distinct from
    /// [`BadRequest`](Self::BadRequest) because the client sent valid JSON and a client
    /// that gets this is version-skewed, not buggy. 400.
    InvalidDecision,
    /// No such addressable thing — a notification id, a proposal `seq`. 404.
    NotFound,
    /// The addressed approval is no longer the pending gate: it was already resolved,
    /// or the loop moved on. **409**, with the *current* pending gate in `detail` so a
    /// client re-renders without a second round-trip. This is the precondition that
    /// closes the "approved a gate that already moved on" race, which today is an
    /// ambiguous 404.
    GateMoved,
    /// The daemon failed at something it was supposed to be able to do (a log append,
    /// a config write, absent shared state). 500.
    Internal,
    /// The TCP bind's auth gate (docs/web-bind.md): no valid cookie or bearer on a
    /// route that needs one. A client acts on this by re-authenticating. 401.
    Unauthorized,
    /// The TCP bind's hardening gate (docs/web-bind.md): a Host or Origin outside the
    /// allowlist. Not a credential problem — re-authenticating won't help. 403.
    Forbidden,
    /// TOTP verification is locked out (docs/web-bind.md); the response carries
    /// `Retry-After`. A client acts by waiting, not by retrying immediately. 429.
    RateLimited,
}

impl ErrorCode {
    /// The canonical HTTP status for this code. One mapping, so two routes returning
    /// the same code can't disagree about the status.
    pub(crate) fn status(self) -> u16 {
        match self {
            Self::BadRequest | Self::InvalidDecision => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::GateMoved => 409,
            Self::RateLimited => 429,
            Self::Internal => 500,
        }
    }
}

/// The body of a non-2xx response.
#[derive(Serialize, Debug)]
pub(crate) struct ErrorBody {
    pub(crate) code: ErrorCode,
    /// The human half — surfaced verbatim by the CLI. Free to change; clients branch on
    /// `code`.
    pub(crate) message: String,
    /// Machine-readable context for codes that carry corrective state — the current
    /// pending gate on a [`ErrorCode::GateMoved`]. Omitted entirely when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<Value>,
}

impl ErrorBody {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    /// Attach corrective state — what the client needs to recover without another
    /// request.
    pub(crate) fn with_detail(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }

    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Answer a conn with a typed error, taking the status from the code so the two can
/// never drift apart.
pub(crate) fn fail(conn: Conn, code: ErrorCode, message: impl Into<String>) -> Conn {
    respond(conn, ErrorBody::new(code, message))
}

/// Answer a conn with a typed error carrying corrective `detail`.
pub(crate) fn respond(conn: Conn, body: ErrorBody) -> Conn {
    conn.with_status(body.code.status())
        .with_body(body.to_json())
}

#[cfg(test)]
mod test;
