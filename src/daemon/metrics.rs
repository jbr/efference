//! Per-session cache/usage telemetry and the keepalive clocks (docs/log-and-projection.md).

use super::*;

/// Per-session cache/usage telemetry. `last_request_at` is an [`Instant`] (not
/// serialized — we surface the derived elapsed seconds, so the non-determinism
/// never reaches a client and there's nothing to keep byte-stable).
#[derive(Default)]
pub(super) struct Metrics {
    pub(super) last_usage: Option<Usage>,
    /// Stamped by **any** request — real turn, fork peek, or keepalive ping. The
    /// cache-touch clock: drives `seconds_since_last_request` and when to ping.
    pub(super) last_request_at: Option<Instant>,
    /// Stamped by real loop turns (incl. the fork) but **not** keepalive pings, so
    /// it measures genuine idle and a ping can't reset its own deadline. Gates the
    /// keepalive cap (docs/log-and-projection.md).
    pub(super) last_real_turn_at: Option<Instant>,
    pub(super) total_input: u64,
    pub(super) total_output: u64,
    pub(super) total_cache_read: u64,
    pub(super) total_cache_creation: u64,
}

/// Session cost telemetry (docs/transport.md): the endpoint whose accounting
/// surface prices requests (armed by [`Shared::enable_cost_tracking`], only when
/// the provider has one — OpenRouter today), the running sum of billed USD, and
/// how many requests couldn't be priced (so the total is legibly an
/// undercount rather than silently wrong).
#[derive(Default)]
pub(super) struct CostCell {
    pub(super) endpoint: Option<crate::api::Endpoint>,
    pub(super) total_usd: f64,
    pub(super) unpriced: u32,
}

/// Which request shape spent the tokens (docs/log-and-projection.md) — the tag on each
/// persisted usage line, so per-shape spend is a query over `usage.jsonl` rather than a
/// reconstruction from the canonical log.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestOrigin {
    /// An agent-loop completion (docs/README.md).
    Turn,
    /// A fork-peek round-trip riding tool results (docs/redaction-fork.md), retries
    /// included.
    Peek,
    /// A standalone idle-triage peek (docs/supervisor.md).
    Triage,
    /// A spotter assessment (docs/permissions.md) — its own prefix, its own lineage.
    Spot,
    /// A cache keepalive ping (docs/log-and-projection.md).
    Keepalive,
}

/// One line of `usage.jsonl`: when, which shape, the upstream's own id for the
/// request, and the four token counts.
#[derive(Serialize)]
struct UsageLine<'a> {
    at_ms: u128,
    origin: RequestOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a str>,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl Shared {
    /// Open (append) the per-request usage record at `path`. Not opening one — tests,
    /// offline verbs — just drops the lines; a failure to open is reported and
    /// likewise degrades to in-memory telemetry only, never a dead daemon.
    pub fn attach_usage_log(&self, path: &std::path::Path) {
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => *self.usage_log.lock().unwrap() = Some(io::BufWriter::new(file)),
            Err(e) => eprintln!("[usage record: could not open {}: {e}]", path.display()),
        }
    }

    /// Append one origin-tagged line to `usage.jsonl` (flushed per line — each is a
    /// completed upstream request worth surviving a crash). Failures are reported and
    /// dropped: the record is diagnostic, never load-bearing.
    fn persist_usage(&self, origin: RequestOrigin, request: &RequestUsage) {
        use std::io::Write;
        let mut sink = self.usage_log.lock().unwrap();
        let Some(sink) = sink.as_mut() else { return };
        let line = UsageLine {
            at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default(),
            origin,
            id: request.id.as_deref(),
            input: request.usage.input_tokens,
            output: request.usage.output_tokens,
            cache_read: request.usage.cache_read_input_tokens,
            cache_write: request.usage.cache_creation_input_tokens,
        };
        let written = serde_json::to_string(&line)
            .map_err(io::Error::other)
            .and_then(|json| writeln!(sink, "{json}").and_then(|()| sink.flush()));
        if let Err(e) = written {
            eprintln!("[usage record: write failed: {e}]");
        }
    }

    /// Record a completed request's usage: persist the origin-tagged line
    /// ([`Self::persist_usage`]), fold it into the telemetry, and stamp the wall
    /// clock. The loop calls this once per API round-trip; `GET /state` reports the
    /// latest plus running totals and the seconds-since (the eviction-prediction
    /// signal, docs/log-and-projection.md). Transport-only — no canonical-log append,
    /// no projection change.
    pub fn record_request(&self, origin: RequestOrigin, request: RequestUsage) {
        self.persist_usage(origin, &request);
        self.spawn_cost_fetch(&request);
        let mut m = self.metrics.lock().unwrap();
        Self::fold_usage(&mut m, request.usage);
        m.last_real_turn_at = Some(Instant::now());
    }

    /// A keepalive ping's usage (docs/log-and-projection.md). Persisted and folded into
    /// the telemetry and the cache-touch clock like any request, but it does **not**
    /// stamp the real-turn clock — a ping must not reset the cap that decides when to
    /// stop pinging.
    pub fn record_keepalive(&self, request: RequestUsage) {
        self.persist_usage(RequestOrigin::Keepalive, &request);
        self.spawn_cost_fetch(&request);
        let mut m = self.metrics.lock().unwrap();
        Self::fold_usage(&mut m, request.usage);
    }

    /// Price one recorded request off the provider's accounting endpoint
    /// (docs/transport.md) — a spawned, best-effort fetch keyed by the same
    /// generation id `persist_usage` just wrote. No-op unless cost tracking is
    /// armed, the request carried an id, and a tokio runtime is present (tests
    /// call the record methods synchronously). The generation record lands
    /// shortly after the response completes and a too-early GET 404s, so two
    /// delayed attempts; exhaustion counts the request `unpriced` — the total
    /// stays a legible undercount instead of a silent one.
    fn spawn_cost_fetch(&self, request: &RequestUsage) {
        let (endpoint, id) = {
            let cell = self.cost.lock().unwrap();
            match (&cell.endpoint, &request.id) {
                (Some(endpoint), Some(id)) => (endpoint.clone(), id.clone()),
                _ => return,
            }
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let cost = Arc::clone(&self.cost);
        handle.spawn(async move {
            for delay in [Duration::from_secs(2), Duration::from_secs(10)] {
                tokio::time::sleep(delay).await;
                match endpoint.fetch_generation_cost(&id).await {
                    Ok(usd) => {
                        cost.lock().unwrap().total_usd += usd;
                        return;
                    }
                    Err(e) => log::debug!("pricing generation {id}: {e}"),
                }
            }
            cost.lock().unwrap().unpriced += 1;
            log::warn!("could not price generation {id}; the session cost undercounts");
        });
    }

    pub(super) fn fold_usage(m: &mut Metrics, usage: Usage) {
        m.total_input += usage.input_tokens;
        m.total_output += usage.output_tokens;
        m.total_cache_read += usage.cache_read_input_tokens;
        m.total_cache_creation += usage.cache_creation_input_tokens;
        m.last_request_at = Some(Instant::now());
        m.last_usage = Some(usage);
    }

    /// `(since last cache-touch, since last real turn)` — the keepalive task's two
    /// clocks. The first decides *when* to ping (the cache is nearing its TTL); the
    /// second enforces the *cap* (a ping doesn't reset it, so unbounded idle still
    /// stops). Both `None` until the first real turn — nothing to keep warm yet.
    pub(super) fn keepalive_clocks(&self) -> (Option<Duration>, Option<Duration>) {
        let m = self.metrics.lock().unwrap();
        (
            m.last_request_at.map(|t| t.elapsed()),
            m.last_real_turn_at.map(|t| t.elapsed()),
        )
    }
}
