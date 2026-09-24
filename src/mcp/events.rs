//! What a call does with server-initiated traffic (docs/tools.md "the client
//! protocol"): [`Session::round_trip`](super::Session::round_trip) classifies
//! each frame and hands the interesting ones here, where they become a live
//! output line or an inbox notification. Nothing here reaches the log —
//! progress is telemetry about the running call, and a `notifications/message`
//! line becomes a [`Notification`] only so the inbox logs it at first-show
//! like every other arrival.

use mcplease::types::{ProgressToken, RequestId};
use serde_json::Value;

use crate::log::Notification;

/// What a server said while a call was in flight.
pub(super) enum ServerEvent {
    /// A `notifications/progress` tick — liveness about the running call.
    Progress {
        progress: f64,
        total: Option<f64>,
        message: Option<String>,
    },
    /// A `notifications/message` log line, sent for the client's logging UI.
    Message {
        level: String,
        logger: Option<String>,
        data: Value,
    },
}

/// Route one event to its sink (docs/tools.md "the client protocol"): a
/// progress tick is one live output line — pre-commit, never projected — and
/// a message is an inbox notification bound to the server's slot, so a burst
/// supersedes itself instead of piling up (docs/inbox.md).
pub(super) fn route_event(
    server: &str,
    event: ServerEvent,
    on_output: &mut (impl FnMut(&[u8]) + Send),
    on_notify: &mut (impl FnMut(Notification) + Send),
) {
    match event {
        ServerEvent::Progress {
            progress,
            total,
            message,
        } => {
            let mut line = match total {
                Some(total) => format!("[progress {progress}/{total}]"),
                None => format!("[progress {progress}]"),
            };
            if let Some(message) = message {
                line.push(' ');
                line.push_str(&message);
            }
            line.push('\n');
            on_output(line.as_bytes());
        }
        ServerEvent::Message {
            level,
            logger,
            data,
        } => {
            let payload = match data.as_str() {
                Some(text) => text.to_string(),
                None => data.to_string(),
            };
            let body = match logger {
                Some(logger) => format!("[{level}] {logger}: {payload}"),
                None => format!("[{level}] {payload}"),
            };
            on_notify(Notification {
                source: server.to_string(),
                body,
                slot: Some(format!("mcp:{server}")),
            });
        }
    }
}

/// Whether a progress token names this request. The token mcplease stamps is
/// the request's own id, so a tick carrying an abandoned call's token is not
/// this call's — rendering it here would put one call's liveness under
/// another's.
pub(super) fn token_matches(id: &RequestId, token: &ProgressToken) -> bool {
    match (id, token) {
        (RequestId::Integer(id), ProgressToken::Integer(token)) => id == token,
        (RequestId::String(id), ProgressToken::String(token)) => id == token,
        _ => false,
    }
}
