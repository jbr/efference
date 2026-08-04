//! The HTTP surface: request/response bodies, the router, and its query helpers (docs/control-channel.md).

use super::*;

#[derive(Deserialize)]
pub(crate) struct TurnBody {
    pub(crate) text: String,
}

/// Body of `POST /background` (docs/supervisor.md): the optional operator note a
/// foreground→background handoff delivers as the next user message.
#[derive(Deserialize)]
pub(crate) struct BackgroundBody {
    #[serde(default)]
    pub(crate) note: Option<String>,
}

/// Body of `POST /proposals/:seq`: the orchestrator's commit/cancel decision on a pending
/// proposal, with an optional rationale note (docs/permissions.md).
#[derive(Deserialize)]
pub(crate) struct ProposalDecisionBody {
    pub(crate) decision: String,
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

/// Body of `POST /pace` (docs/control-channel.md): an operator pace change. Both
/// fields optional so a client can set the speed dial, toggle the hold, or both in one call —
/// `{"speed_ms":10000}` slows, `{"held":true}` pauses, `{"held":false}` resumes.
#[derive(Deserialize)]
pub(crate) struct PaceBody {
    #[serde(default)]
    pub(crate) speed_ms: Option<u64>,
    #[serde(default)]
    pub(crate) held: Option<bool>,
}

/// Body of `POST /review-level` (docs/permissions.md): the operator sets the
/// review-level dial to one of `autonomous` / `spotter_human` / `human_only`.
#[derive(Deserialize)]
pub(crate) struct ReviewLevelBody {
    pub(crate) level: String,
}

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

pub(crate) const MISSING_STATE: &str = r#"{"error":"missing shared state"}"#;

/// Pull the shared daemon state out of a conn's server-global TypeSet, or short
/// out with a 500 if it's somehow absent (a wiring bug, never seen at runtime).
pub(crate) fn shared_of(conn: &Conn) -> Option<Arc<Shared>> {
    conn.shared_state::<Arc<Shared>>().map(Arc::clone)
}

/// The HTTP surface — a `trillium-router` over the verb set (docs/control-channel.md). Each
/// handler reads `Shared` from the server-global state; JSON in, JSON out. The
/// router gives method-aware dispatch (and a clean `:call_id` capture) in place
/// of the earlier hand-rolled `(method, path)` match.
pub(crate) fn router() -> Router {
    Router::new()
        // Enqueue a user turn (also seeds turn 1). 202 — watch /log (and the
        // future /events) for the response; submission is decoupled from inference.
        .post("/turn", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let wait = wants_wait(conn.querystring());
            let body = conn.request_body_string().await.unwrap_or_default();
            match serde_json::from_str::<TurnBody>(&body) {
                Ok(TurnBody { text }) => {
                    shared.submit_turn(text);
                    // `?wait`: hold the connection until the turn reaches a gate or completes,
                    // then answer with the resulting state — "act → read state" in one round-trip
                    // (docs/permissions.md).
                    if wait {
                        settle_and_state(&shared, conn).await
                    } else {
                        conn.with_status(202).with_body(r#"{"status":"accepted"}"#)
                    }
                }
                Err(e) => conn
                    .with_status(400)
                    .with_body(format!(r#"{{"error":"bad /turn body: {e}"}}"#)),
            }
        })
        // Resolve a pending approval gate by its tool_use id.
        .post("/approvals/:call_id", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let id = CallId(conn.param("call_id").unwrap_or_default().to_string());
            let wait = wants_wait(conn.querystring());
            let body = conn.request_body_string().await.unwrap_or_default();
            let decision = match serde_json::from_str::<DecisionBody>(&body) {
                Ok(d) => d,
                Err(e) => {
                    return conn
                        .with_status(400)
                        .with_body(format!(r#"{{"error":"bad decision body: {e}"}}"#));
                }
            };
            let approval = match decision.decision.as_str() {
                "approve" => Approval::Approve,
                "abort" => Approval::Abort,
                "redirect" => Approval::Redirect(decision.message.unwrap_or_default()),
                "config_edit" => match decision.change {
                    Some(change) => Approval::ConfigEdit {
                        change,
                        note: decision.note.unwrap_or_default(),
                    },
                    None => {
                        return conn
                            .with_status(400)
                            .with_body(r#"{"error":"config_edit decision requires a change"}"#);
                    }
                },
                other => {
                    return conn
                        .with_status(400)
                        .with_body(format!(r#"{{"error":"unknown decision: {other}"}}"#));
                }
            };
            if shared.resolve_approval(&id, approval) {
                // `?wait`: hold until the turn reaches its *next* gate or completes — so a
                // multi-gate turn is driven as a clean "approve → read state" loop.
                if wait {
                    settle_and_state(&shared, conn).await
                } else {
                    conn.with_status(200).with_body(r#"{"status":"resolved"}"#)
                }
            } else {
                conn.with_status(404)
                    .with_body(r#"{"error":"no pending approval with that id"}"#)
            }
        })
        .post("/interrupt", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            shared.interrupt();
            conn.with_status(200)
                .with_body(r#"{"status":"interrupted"}"#)
        })
        // Detach the running foreground tool into the background supervisor
        // (docs/supervisor.md, the foreground→background handoff) instead of
        // killing it. Optional body `{"note": "…"}` — delivered as the next user
        // message alongside the handoff. 202: the request only lands if a
        // foreground tool is actually mid-run; otherwise it is a no-op.
        // (Interim verb-route; the RESTful form is
        // backlog/processes-rest-resource.md.)
        .post("/background", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let body = conn.request_body_string().await.unwrap_or_default();
            let note = if body.trim().is_empty() {
                None
            } else {
                match serde_json::from_str::<BackgroundBody>(&body) {
                    Ok(b) => b.note,
                    Err(e) => {
                        return conn
                            .with_status(400)
                            .with_body(format!(r#"{{"error":"bad /background body: {e}"}}"#));
                    }
                }
            };
            shared.request_detach(note);
            conn.with_status(202)
                .with_body(r#"{"status":"detach requested"}"#)
        })
        // Set the operator's pace dial / hold overlay (docs/control-channel.md).
        // Operator-view only — never a log entry, so it can't perturb the wire or the cache.
        // Returns the resulting pace so the client reflects it without a follow-up `/state`.
        .post("/pace", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let body = conn.request_body_string().await.unwrap_or_default();
            match serde_json::from_str::<PaceBody>(&body) {
                Ok(p) => {
                    let (speed, held) =
                        shared.set_pace(p.speed_ms.map(Duration::from_millis), p.held);
                    conn.with_status(200).with_body(format!(
                        r#"{{"speed_ms":{},"held":{}}}"#,
                        speed.as_millis() as u64,
                        held
                    ))
                }
                Err(e) => conn
                    .with_status(400)
                    .with_body(format!(r#"{{"error":"bad /pace body: {e}"}}"#)),
            }
        })
        // Set the review-level dial (docs/permissions.md): who reviews a
        // gated call. Runtime-mutable, reported on `GET /state`. Returns the resulting level.
        .post("/review-level", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let body = conn.request_body_string().await.unwrap_or_default();
            match serde_json::from_str::<ReviewLevelBody>(&body) {
                Ok(b) => match crate::policy::ReviewLevel::parse(&b.level) {
                    Some(level) => {
                        shared.set_review_level(level);
                        conn.with_status(200)
                            .with_body(format!(r#"{{"review_level":"{}"}}"#, level.as_str()))
                    }
                    None => conn.with_status(400).with_body(
                        r#"{"error":"level must be autonomous | spotter_human | human_only"}"#,
                    ),
                },
                Err(e) => conn
                    .with_status(400)
                    .with_body(format!(r#"{{"error":"bad /review-level body: {e}"}}"#)),
            }
        })
        // Enqueue an ambient notification into the inbox (docs/inbox.md). 202 — it waits
        // for the next triage gate (Slice C); a client watches /state for the inbox.
        .post("/notify", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let body = conn.request_body_string().await.unwrap_or_default();
            match serde_json::from_str::<NotifyBody>(&body) {
                Ok(n) => {
                    let id = shared.notify(n.source, n.body, n.slot, n.replaces);
                    conn.with_status(202).with_body(format!(r#"{{"id":{id}}}"#))
                }
                Err(e) => conn
                    .with_status(400)
                    .with_body(format!(r#"{{"error":"bad /notify body: {e}"}}"#)),
            }
        })
        // Dismiss a pending notification out-of-band by its queue-local id (docs/inbox.md).
        .post("/notifications/:id/dismiss", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            match conn.param("id").and_then(|s| s.parse::<u64>().ok()) {
                Some(id) if shared.dismiss_notification(id) => {
                    conn.with_status(200).with_body(r#"{"status":"dismissed"}"#)
                }
                Some(_) => conn
                    .with_status(404)
                    .with_body(r#"{"error":"no pending notification with that id"}"#),
                None => conn
                    .with_status(400)
                    .with_body(r#"{"error":"notification id must be an integer"}"#),
            }
        })
        // List async config-edit proposals awaiting an orchestrator decision
        // (docs/permissions.md). The same array `/state` carries; a
        // dedicated GET lets a client poll just this without the rest of the snapshot.
        .get("/proposals", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let body =
                serde_json::to_string(&shared.pending_proposals()).unwrap_or_else(|_| "[]".into());
            conn.with_status(200).with_body(body)
        })
        // Commit or cancel a proposal by its `seq` (the `Proposed` entry's id). Body:
        // `{"decision":"commit"|"cancel","note":"…"}`. Commit folds the proposed change into
        // the effective config (and notifies the session for scope/paths); cancel is
        // audit-only. Either way the proposal leaves the pending queue. The orchestrator-above
        // legislates here — the session can only *file* a proposal, never resolve its own.
        .post("/proposals/:seq", |mut conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let Some(seq) = conn.param("seq").and_then(|s| s.parse::<u64>().ok()) else {
                return conn
                    .with_status(400)
                    .with_body(r#"{"error":"proposal seq must be an integer"}"#);
            };
            let raw = conn.request_body_string().await.unwrap_or_default();
            let decision = match serde_json::from_str::<ProposalDecisionBody>(&raw) {
                Ok(d) => d,
                Err(e) => {
                    return conn
                        .with_status(400)
                        .with_body(format!(r#"{{"error":"bad /proposals body: {e}"}}"#));
                }
            };
            let commit = match decision.decision.as_str() {
                "commit" => true,
                "cancel" => false,
                other => {
                    return conn.with_status(400).with_body(format!(
                        r#"{{"error":"decision must be \"commit\" or \"cancel\", got {other:?}"}}"#
                    ));
                }
            };
            match shared.resolve_proposal_decision(seq, commit, decision.note.unwrap_or_default()) {
                Ok(true) => conn.with_status(200).with_body(if commit {
                    r#"{"status":"committed"}"#
                } else {
                    r#"{"status":"cancelled"}"#
                }),
                Ok(false) => conn
                    .with_status(404)
                    .with_body(r#"{"error":"no pending proposal with that seq"}"#),
                Err(e) => conn
                    .with_status(500)
                    .with_body(format!(r#"{{"error":"append failed: {e}"}}"#)),
            }
        })
        .get("/state", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            conn.with_status(200).with_body(shared.state_json())
        })
        // Long-poll: block until the daemon reaches a decision point (a gate is pending, or the
        // loop is idle), then return the `/state` snapshot. Lets a client drive the daemon
        // synchronously — "act, read state, act" — instead of polling `/state` in a wall-clock
        // loop (docs/permissions.md). Returns immediately if already settled.
        .get("/settle", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            settle_and_state(&shared, conn).await
        })
        .get("/log", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let since = parse_since(conn.querystring());
            conn.with_status(200).with_body(shared.log_json(since))
        })
        // Live tail of the append-only log (docs/README.md — the log *is* the event
        // store). `?since=<seq>` replays from there then follows; default 0 streams
        // the whole session and then live. Each event carries `id: <seq>`, so a
        // reconnecting client resumes with `?since=<last_seq + 1>` — the explicit
        // cursor the SSE crate's bare framing leaves to us.
        .get("/events", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            let since = parse_since(conn.querystring()).unwrap_or(0);
            conn.with_sse_stream(tail_events(shared, since))
        })
        // Live tail of *ephemeral* output deltas (docs/README.md): assistant prose and
        // tool output as they're produced, before the turn commits. No `since`
        // cursor and no replay — these carry no `seq` and aren't durable; a client
        // that wants the authoritative record tails `/events`. A late subscriber
        // simply starts from the next delta.
        .get("/stream", |conn: Conn| async move {
            let Some(shared) = shared_of(&conn) else {
                return conn.with_status(500).with_body(MISSING_STATE);
            };
            conn.with_sse_stream(stream_deltas(shared))
        })
}

/// Pull `since=<n>` out of a raw query string (`GET /log?since=12`).
pub(crate) fn parse_since(query: &str) -> Option<u64> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("since="))
        .and_then(|v| v.parse().ok())
}

/// Whether a request asked to block until settled (`?wait`, `?wait=1`, `?wait=true`) — the
/// long-poll opt-in on `/turn` and `/approvals` (docs/permissions.md).
pub(crate) fn wants_wait(query: &str) -> bool {
    query
        .split('&')
        .any(|kv| matches!(kv, "wait" | "wait=1" | "wait=true"))
}

/// Block until the daemon settles (capped by [`SETTLE_TIMEOUT`]), then answer with the `/state`
/// snapshot — the shared tail of `GET /settle` and the `?wait` paths. The body is exactly `/state`'s,
/// so a settled response is just a fresh state read; the client inspects `gate` to tell a
/// gate from idle.
pub(crate) async fn settle_and_state(shared: &Arc<Shared>, conn: Conn) -> Conn {
    let _ = tokio::time::timeout(SETTLE_TIMEOUT, shared.await_settled()).await;
    conn.with_status(200).with_body(shared.state_json())
}
