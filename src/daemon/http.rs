//! The control API surface: request bodies, the route table, and its query helpers (docs/control-channel.md). The routes themselves are the `http/` submodules.

use super::*;

#[derive(Deserialize)]
pub(crate) struct TurnBody {
    pub(crate) text: String,
    /// The client's identifier for this submission (docs/control-api.md),
    /// adopted when present and daemon-minted otherwise — the conn-id
    /// adopt-or-mint rule at submission granularity. Echoed in the 202 body
    /// and recorded on the resulting `User` entry as `submission_id`.
    #[serde(default)]
    pub(crate) submission_id: Option<String>,
    /// The client's self-chosen label for who is submitting (docs/control-api.md):
    /// `web`, `mcp[<name>]`, or `agent[<id>]`. Client-asserted, like a
    /// notification's source. Absent = the operator, whose turns carry no prefix.
    /// Recorded on the resulting `User` entry as `source` and projected onto its
    /// wire block when present.
    #[serde(default)]
    pub(crate) source: Option<String>,
    /// Names of files this client uploaded (`POST /attachments`) to send with the
    /// turn (docs/inbox.md). Each must still be in the store; the turn is refused
    /// otherwise, so a message never lands claiming a file that is not there.
    #[serde(default)]
    pub(crate) attachments: Vec<String>,
}

/// The optional `POST /loop/compact` body: `warm` is the operator's choice to compact a
/// session whose cache is not believed expired (docs/control-api.md).
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompactBody {
    #[serde(default)]
    pub(crate) warm: bool,
}

/// Cap on a client-minted submission id: long enough for any uuid-ish scheme,
/// short enough that the log can't be bloated through the field.
const SUBMISSION_ID_MAX: usize = 128;

/// Mint a submission id for a `POST /v1/turns` whose body didn't carry one, so
/// every submitted turn is identified in the log regardless of client
/// (docs/control-api.md). Public because a client that wants its submission
/// addressable — for the delivery echo, or to recall it — mints its own with
/// the same scheme rather than inventing a second one (docs/tui.md).
pub fn mint_submission_id() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Body of `PATCH /processes/:id` (docs/process-monitor.md, docs/control-api.md): the
/// settable properties of a process resource. `background: true` on the foreground
/// entry is the foreground→background handoff; `note` is the operator note the
/// handoff delivers as the next user message — a turn-injection concern riding
/// beside the property change, never a process attribute.
#[derive(Deserialize)]
pub(crate) struct ProcessPatchBody {
    #[serde(default)]
    pub(crate) background: Option<bool>,
    #[serde(default)]
    pub(crate) muted: Option<bool>,
    #[serde(default)]
    pub(crate) note: Option<String>,
}

/// Body of `PATCH /proposals/:seq`: the orchestrator moves a pending proposal to its
/// terminal state (`committed` | `cancelled`), with an optional rationale note
/// (docs/permissions.md).
#[derive(Deserialize)]
pub(crate) struct ProposalPatchBody {
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) note: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct DecisionBody {
    pub(crate) decision: String,
    #[serde(default)]
    pub(crate) message: Option<String>,
    /// The config-edit payload (`decision: "config_edit"` only) — the change and a rationale note
    /// (docs/permissions.md). A committed change is always written to the session's
    /// config file, so there is no lifetime to choose; a `lifetime` field from an older client is
    /// ignored rather than rejected.
    #[serde(default)]
    pub(crate) change: Option<crate::log::ConfigChange>,
    #[serde(default)]
    pub(crate) note: Option<String>,
}

/// Body of `PATCH /loop` (docs/control-channel.md): the operator dials, each
/// optional so one call can move either or both. `pace` is itself partial —
/// `{"pace":{"speed_ms":10000}}` slows, `{"pace":{"held":true}}` pauses,
/// `{"pace":{"held":false}}` resumes; `review_level` is one of `autonomous` /
/// `supervisor` / `spotter_human` / `human_only` (docs/permissions.md).
#[derive(Deserialize)]
pub(crate) struct LoopPatchBody {
    #[serde(default)]
    pub(crate) pace: Option<PaceBody>,
    #[serde(default)]
    pub(crate) review_level: Option<String>,
}

/// The `pace` half of [`LoopPatchBody`].
#[derive(Deserialize)]
pub(crate) struct PaceBody {
    #[serde(default)]
    pub(crate) speed_ms: Option<u64>,
    #[serde(default)]
    pub(crate) held: Option<bool>,
}

/// Body of `DELETE /v1/push/subscriptions` (docs/web-push.md): the endpoint to
/// forget, which is the subscription's identity.
#[derive(Deserialize)]
pub(crate) struct PushUnsubscribeBody {
    pub(crate) endpoint: String,
}

/// What every push route answers when no machine root is injected: the VAPID
/// keypair derives from it (`auth.rs`), so without one there is no sender
/// identity to hand out or sign with. Unreachable from a browser, which can only
/// reach the daemon over the TCP bind, and that bind requires the root.
const NO_ROOT: &str = "no root is installed, so push has no signing key";

#[derive(Deserialize)]
pub(crate) struct NotifyBody {
    pub(crate) source: String,
    pub(crate) body: String,
    /// Logical channel — a later notify on the same slot supersedes this one (docs/inbox.md).
    #[serde(default)]
    pub(crate) slot: Option<String>,
    /// Supersede a specific pending notification by its queue-local id.
    #[serde(default)]
    pub(crate) replaces: Option<u64>,
}

/// The session a conn addresses: the one a host's `/sessions/:id` stage put into
/// conn state, else the server-global one a session bind registered
/// (docs/control-channel.md). `None` only on a wiring bug, answered as a 500.
pub(crate) fn shared_of(conn: &Conn) -> Option<Arc<Session>> {
    conn.state::<Arc<Session>>()
        .or_else(|| conn.shared_state::<Arc<Session>>())
        .map(Arc::clone)
}

/// The answer when the server-global state is absent — the one condition every route
/// shares, and the reason it has a helper rather than a body constant.
fn missing_state(conn: Conn) -> Conn {
    fail(conn, ErrorCode::Internal, "missing shared state")
}

/// How long `/events` may sit silent before it sends an SSE comment (docs/control-api.md).
///
/// An idle stream through a TLS intermediary gets reaped, and a client cannot otherwise
/// distinguish idle from dead; a comment (`: `) is the one keepalive an `EventSource`
/// consumer ignores — every shape `Eventable` can encode fires `onmessage` — and the one
/// that stays out of the frame envelope, since a heartbeat *frame* would need a fourth
/// `class` naming no durability. It is also what makes `?since` past the head safe to
/// serve as a wait rather than an error. (`daemon/keepalive.rs` is the *upstream* cache
/// ping; unrelated.)
pub(crate) const EVENTS_HEARTBEAT: Duration = Duration::from_secs(15);

mod attachments;
mod events;
mod log_reads;
mod loop_control;
mod notifications;
mod processes;
mod proposals;
mod state;
mod title;
mod turns;
mod web_push;

/// The control API (docs/control-channel.md): every `(method, path)` the daemon answers,
/// as one `Router`. Each route is a named `async fn` in a sibling module under `http/`;
/// they read `Session` from the server-global state, JSON in, JSON out. The router
/// supplies method-aware dispatch and the path captures (`:call_id`, `:id`, `:seq`).
/// Mounted only by [`handler`](super::handler::handler).
///
/// `heartbeat` is the `/events` idle-comment interval — [`EVENTS_HEARTBEAT`] via
/// [`control_routes`] everywhere real; a parameter so a test can shrink it and watch a
/// quiet stream stay audibly alive.
pub(crate) fn control_routes_with_heartbeat(heartbeat: Duration) -> Router {
    Router::new()
        .post("/v1/turns", turns::submit_turn)
        .delete("/v1/turns/:submission_id", turns::withdraw_turns)
        .put("/v1/approvals/:call_id", turns::resolve_approval)
        .post("/v1/loop/interrupt", loop_control::interrupt)
        .post("/v1/loop/continue", loop_control::continue_loop)
        .post("/v1/loop/compact", loop_control::compaction)
        .get("/v1/loop", loop_control::read_loop)
        .patch("/v1/loop", loop_control::patch_loop)
        .get("/v1/processes", processes::list_processes)
        .get("/v1/processes/:id", processes::read_process)
        .get("/v1/processes/:id/output", processes::read_process_output)
        .patch("/v1/processes/:id", processes::patch_process)
        .delete("/v1/processes/:id", processes::kill_process)
        .post("/v1/attachments", attachments::upload)
        .get("/v1/attachments/:name", attachments::read)
        .delete("/v1/attachments/:name", attachments::remove)
        .post("/v1/notifications", notifications::notify)
        .delete("/v1/notifications/:id", notifications::dismiss_notification)
        .get("/v1/proposals", proposals::list_proposals)
        .patch("/v1/proposals/:seq", proposals::patch_proposal)
        .put("/v1/title", title::put_title)
        .get("/v1/state", state::read_state)
        .get("/v1/log", log_reads::read_log_window)
        .get("/v1/transcript", log_reads::read_transcript)
        .get("/v1/log/:seq", log_reads::read_log_entry)
        .get("/v1/log/:seq/images/:idx", log_reads::read_result_image)
        .get("/v1/tool_calls/:call_id", log_reads::read_tool_call)
        .get("/v1/tools/:name", log_reads::read_tool_definition)
        .get("/v1/push", web_push::read_push_key)
        .post("/v1/push/subscriptions", web_push::subscribe)
        .delete("/v1/push/subscriptions", web_push::unsubscribe)
        .get("/v1/events", events::events(heartbeat))
}

/// [`control_routes_with_heartbeat`] at the real [`EVENTS_HEARTBEAT`] interval.
pub(crate) fn control_routes() -> Router {
    control_routes_with_heartbeat(EVENTS_HEARTBEAT)
}

/// Which window of the log a `GET /log` read wants (docs/control-api.md).
///
/// `since`/`before` bound the range; `tail` takes the last N *of that range*. Splitting
/// position from count is why `?tail=N` is its own parameter rather than a negative
/// `since`: a position and a count are different kinds, and a parser cannot tell a
/// deliberate `since=-300` from a client's off-by-one `-1`.
///
/// Three reads follow from the three fields: `?since=S` resumes forward, `?tail=N` opens
/// cold at the tail, and `?before=X&tail=N` is one page backward — the escape hatch for a
/// client scrolling past the window it opened with.
#[derive(Default, PartialEq, Eq, Debug)]
pub(crate) struct LogSelect {
    /// Inclusive lower bound: entries with `seq >= since`.
    pub(crate) since: Option<u64>,
    /// Exclusive upper bound: entries with `seq < before`.
    pub(crate) before: Option<u64>,
    /// Keep only the last `tail` entries of the bounded range. Exactly N, cut anywhere:
    /// a cut can orphan a reference (a `tool_result` whose call sits in the entry before
    /// the cut), and those resolve by fetching `/log/:seq` or `/tool_calls/:call_id`
    /// rather than by widening the window.
    pub(crate) tail: Option<u64>,
}

impl LogSelect {
    /// Parse the `GET /log` query, or say what was wrong with it.
    ///
    /// `since` and `tail` together are an **error**, not a resolved precedence: one asks
    /// to resume from a position and the other to open at the end, so a request carrying
    /// both has no single reading, and picking one silently would answer a question the
    /// client didn't ask.
    pub(crate) fn parse(query: &str) -> Result<Self, String> {
        let select = Self {
            since: parse_u64_param(query, "since")?,
            before: parse_u64_param(query, "before")?,
            tail: parse_u64_param(query, "tail")?,
        };
        if select.since.is_some() && select.tail.is_some() {
            return Err("since and tail are mutually exclusive".into());
        }
        Ok(select)
    }
}

/// One `name=<u64>` query parameter, absent (`Ok(None)`) or malformed (`Err`).
///
/// Unlike [`parse_since`], a value that doesn't parse is rejected rather than ignored: on
/// a windowed read, a dropped bound silently returns the *whole log*, which is the
/// failure the window exists to prevent.
fn parse_u64_param(query: &str, name: &str) -> Result<Option<u64>, String> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
        .map(|v| {
            v.parse()
                .map_err(|_| format!("{name} must be a non-negative integer, got {v:?}"))
        })
        .transpose()
}

/// One query parameter, percent-decoded, or `None` if absent. For values a
/// client encodes (paths, connection names); [`parse_param`] is the raw slice.
pub(crate) fn query_value(query: &str, name: &str) -> Option<String> {
    trillium_client::url::form_urlencoded::parse(query.as_bytes())
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
}

/// One `name=<value>` query parameter as a raw string slice, or `None` if absent.
pub(crate) fn parse_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
}

/// Pull `since=<n>` out of a raw query string (`GET /events?since=12`).
pub(crate) fn parse_since(query: &str) -> Option<u64> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("since="))
        .and_then(|v| v.parse().ok())
}

/// Whether a request asked to block until settled (`?wait`, `?wait=1`, `?wait=true`) — the
/// long-poll opt-in on the write routes (`/turns`, `/approvals`) (docs/permissions.md).
pub(crate) fn wants_wait(query: &str) -> bool {
    query
        .split('&')
        .any(|kv| matches!(kv, "wait" | "wait=1" | "wait=true"))
}

/// Block until the daemon settles (capped by [`SETTLE_TIMEOUT`]), then answer with the `/state`
/// snapshot — the shared tail of `GET /state?wait=settled` and the write routes' `?wait`. The
/// body is exactly `/state`'s, so a settled response is just a fresh state read; the client
/// inspects `gate` to tell a gate from idle.
pub(crate) async fn settle_and_state(shared: &Arc<Session>, conn: Conn) -> Conn {
    let _ = tokio::time::timeout(SETTLE_TIMEOUT, shared.await_settled()).await;
    conn.with_status(200).with_body(shared.state_json()).halt()
}

#[cfg(test)]
mod test {
    use super::query_value;

    #[test]
    fn query_value_decodes_the_named_value() {
        let query = "connection=open%20router&cwd=%2FUsers%2Fjbr%2Fcode";
        assert_eq!(query_value(query, "connection").unwrap(), "open router");
        assert_eq!(query_value(query, "cwd").unwrap(), "/Users/jbr/code");
        assert_eq!(query_value(query, "config"), None);
    }
}
