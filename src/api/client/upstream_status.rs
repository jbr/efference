//! What the transport observes on every upstream response, below the agent
//! loop: the status diagnostic, the stashed error body, and the per-attempt
//! record (docs/transport.md).

use super::*;

/// Record one non-success upstream status. Matching statuses increment the
/// diagnostic's count; a different one starts a new run (docs/transport.md).
pub(in crate::api) fn record_upstream_failure(
    failure: &UpstreamFailureState,
    status: Option<u16>,
    detail: impl Into<String>,
) {
    let mut failure = failure.lock().unwrap();
    let mut next = UpstreamFailure::new(status, detail);
    if let Some(previous) = failure
        .as_ref()
        .filter(|previous| previous.status == status)
    {
        next.count = previous.count.saturating_add(1);
    }
    *failure = Some(next);
}

/// The all-clear: a 2xx means the upstream is answering, so the chip stops
/// reporting a failure that is over (docs/transport.md). This is the non-retry
/// half of the boundary — a retry that finalizes arrives here too, because
/// finalizing required a 2xx.
pub(in crate::api) fn clear_upstream_failure(failure: &UpstreamFailureState) {
    *failure.lock().unwrap() = None;
}

/// A non-2xx response body, read in the reverse pass and stashed on the conn.
///
/// Reading a body consumes it, and the caller that would otherwise read it — the
/// `into_sse` open error, or `keepalive` — runs *after* this handler. Parking the
/// bytes in the conn's state is what lets both the durable per-attempt record and
/// the caller's own error message quote the upstream, instead of one of them
/// finding an empty body.
pub(super) struct CapturedErrorBody(pub(super) String);

/// A non-2xx response's body: the copy [`UpstreamStatusHandler`] stashed, or —
/// where it stashed none — the body read from the conn now.
pub(super) async fn read_error_body(conn: &mut Conn) -> trillium_client::Result<String> {
    match conn.take_state::<CapturedErrorBody>() {
        Some(body) => Ok(body.0),
        None => conn.response_body().read_string().await,
    }
}

/// Observes every upstream response in the reverse pass, before the outer retry
/// handler queues another attempt (docs/transport.md).
///
/// Three things happen here, all of them below the agent loop:
///
/// - a non-2xx updates the status/count diagnostic; a 2xx clears it, which is the
///   chip's all-clear boundary;
/// - a non-2xx body is read once and stashed, both for the caller and for the record;
/// - the failed attempt is queued for the loop to drain into a durable record-only
///   log entry — the handler cannot append, so the body crosses that boundary
///   through the queue.
pub(super) struct UpstreamStatusHandler {
    pub(super) failure: UpstreamFailureState,
    pub(super) attempts: UpstreamAttemptQueue,
    /// The turn's sticky-routing token and the header it arrives on
    /// (docs/transport.md); `None` where the provider has no such contract.
    pub(super) turn_routing: Option<(&'static str, TurnRoutingState)>,
    /// The subscription-window snapshot (`api/plan.rs`), on the provider that
    /// reports one. Read on every status: the 429 that ends a window carries it.
    pub(super) plan: Option<PlanState>,
    /// The request whose attempt this is, when stages are recorded
    /// (`api/stages.rs`).
    pub(super) active: ActiveTrace,
}

impl ClientHandler for UpstreamStatusHandler {
    async fn after_response(&self, conn: &mut Conn) -> trillium_client::Result<()> {
        let Some(status) = conn.status() else {
            return Ok(());
        };
        if let Some(cell) = &self.plan {
            let headers = conn.response_headers();
            let observed_at = crate::log::Timestamp::now().0;
            let names: Vec<String> = headers
                .iter()
                .map(|(name, _)| name.as_ref().to_string())
                .collect();
            if let Some(snapshot) = plan_from_headers(
                names,
                |name| headers.get_str(name).map(str::to_string),
                observed_at,
            ) {
                *cell
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(snapshot);
            }
        }
        if status.is_success() {
            clear_upstream_failure(&self.failure);
            // First token of the turn wins: the backend issues it at turn start and
            // the contract is to replay that value unchanged until the turn ends.
            if let Some((header, cell)) = &self.turn_routing
                && let Some(token) = conn.response_headers().get_str(*header)
            {
                let mut slot = cell
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if slot.is_none() {
                    *slot = Some(token.to_string());
                }
            }
            return Ok(());
        }
        let status = u16::from(status);
        let active = self.active.lock().unwrap().clone();
        if let Some(active) = active {
            active.attempt_failed(status);
        }
        let body = conn
            .response_body()
            .read_string()
            .await
            .unwrap_or_else(|read_err| format!("(could not read error body: {read_err})"));
        record_upstream_failure(&self.failure, Some(status), &body);
        conn.insert_state(CapturedErrorBody(body.clone()));
        push_upstream_attempt(&self.attempts, UpstreamAttemptFailure { status, body });
        Ok(())
    }
}

/// The status and allowlisted response headers of an executed conn
/// (docs/transport.md).
///
/// The allowlist is [`is_captured_header`]: a response header can carry
/// account-scoped values, so what may reach the log is a named set rather than
/// whatever the edge attached (docs/env.md). Values still pass the `[env]` masker
/// at append, like every other stored string.
pub(super) fn response_meta(conn: &Conn) -> ResponseMeta {
    let headers = conn
        .response_headers()
        .into_iter()
        .filter_map(|(name, values)| {
            let name = name.to_string();
            let value = values.one()?.as_str()?;
            is_captured_header(&name).then(|| (name.to_ascii_lowercase(), value.to_string()))
        })
        .collect();
    ResponseMeta {
        status: conn.status().map_or(0, u16::from),
        headers,
    }
}
