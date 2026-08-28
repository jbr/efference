//! Per-session cache/usage telemetry and the keepalive clocks (docs/log-and-projection.md).

use super::*;

/// Per-session cache/usage telemetry.
///
/// **Two clocks, two jobs.** The [`Instant`]s are monotonic and drive *interval* logic
/// (when to ping, when the keepalive cap is reached) — the right clock for that, since an
/// NTP step must not move a deadline. [`Metrics::last_request_wall`] is the wall-clock
/// stamp clients read.
///
/// The wall stamp is **absolute, not elapsed**, and that is load-bearing rather than
/// cosmetic (docs/control-api.md): a field that ticks with the clock changes on every
/// read, so under level-triggered emission "emit when it changes" becomes "emit forever."
/// Sending the instant and letting clients subtract keeps `usage` quiet until a request
/// actually happens.
#[derive(Default)]
pub(super) struct Metrics {
    pub(super) last_usage: Option<Usage>,
    /// The last **main-loop** completion's usage, stamped by `RequestOrigin::Turn`
    /// alone. Context-window occupancy is a fact about the trajectory's own
    /// prefix, and the side channels do not share it: a fork peek, a triage, or a
    /// spotter assessment carries its own much smaller prompt, so reading
    /// occupancy off [`Metrics::last_usage`] makes the reported context *drop*
    /// mid-turn and correct when the turn completes. Side-channel usage still
    /// belongs in the cumulative totals and the cost, which is why this is a
    /// second field rather than a filter on the first.
    pub(super) last_turn_usage: Option<Usage>,
    /// Stamped by **any** request — real turn, fork peek, or keepalive ping. The
    /// cache-touch clock: decides when to ping.
    pub(super) last_request_at: Option<Instant>,
    /// The same event as [`Metrics::last_request_at`], on the wall clock, for the wire.
    pub(super) last_request_wall: Option<crate::log::Timestamp>,
    /// Stamped by real loop turns (incl. the fork) but **not** keepalive pings, so
    /// it measures genuine idle and a ping can't reset its own deadline. Gates the
    /// keepalive cap (docs/log-and-projection.md).
    pub(super) last_real_turn_at: Option<Instant>,
    pub(super) total_input: u64,
    pub(super) total_output: u64,
    pub(super) total_cache_read: u64,
    pub(super) total_cache_creation: u64,
    /// Consecutive main-loop requests that read nothing from the cache and had no
    /// legitimate reason to (docs/log-and-projection.md) — the live end of the
    /// empirical prefix oracle, surfaced by `GET /state` as `usage.cold_streak`.
    pub(super) cold_streak: ColdStreak,
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
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
    /// A retried attempt whose stream aborted before finalizing — record-only,
    /// all-zero usage (docs/transport.md). Persisted as its own origin so it is not
    /// read as a real main-loop request: it must not be priced
    /// ([`Shared::spawn_cost_fetch`]) and must not count as prefix evidence
    /// ([`ColdStreak::observe`]).
    AbortedAttempt,
    /// An origin this build does not recognize — a forward-compat catch-all so a
    /// `usage.jsonl` line written by a newer build still parses here (and, per
    /// serde's `#[serde(other)]` contract, round-trips as this variant's own name
    /// rather than the original one if ever re-serialized).
    #[serde(other)]
    Unknown,
}

/// How long the main-loop prefix may go untouched before a cold read on the next
/// turn is explained by eviction rather than by a moved prefix — Anthropic's
/// five-minute ephemeral TTL (docs/transport.md). The keepalive pings under it
/// while a session idles, so a gap this long means the pings also stopped (the cap)
/// or never ran.
const CACHE_TTL: Duration = Duration::from_secs(300);

/// How many consecutive unexplained cold main-loop requests before the streak is
/// worth surfacing (docs/log-and-projection.md). Four, from a fold over this repo's
/// own corpus (77 `usage.jsonl` files, 44 with turns, 1371 `turn` requests):
/// 155 of those turns read nothing from the cache, and this
/// classifier explains 54 of them (38 session boot, 9 idle past the TTL, 7 a model
/// change). The remaining 101 fall into 28 runs — 24 of length 1, two of length 3,
/// and two of length 28 and 43, each of which was a whole session that never once
/// read the cache. So runs of 1–3 are what normal operation produces (a retry that
/// re-sent a prefix, an eviction this heuristic misses), and the defect signal is an
/// order of magnitude longer; 4 sits in the empty band between them.
pub const COLD_STREAK_WARN: u32 = 4;

/// Why a cold main-loop read needed no explanation from the prefix
/// (docs/log-and-projection.md) — the three conditions under which the provider is
/// expected to have nothing cached, derivable from the usage record alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColdStart {
    /// The session's first request: nothing was written yet.
    Boot,
    /// The model changed; a different model does not share a cached prefix.
    ModelChange,
    /// The prefix went untouched past [`CACHE_TTL`] — evicted, not moved.
    Idle,
}

/// What one main-loop request's `cache_read_input_tokens` says about the prefix
/// (docs/log-and-projection.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheRead {
    /// The provider read a cached prefix: the bytes still match.
    Warm,
    /// Cold, with a reason that is not the prefix ([`ColdStart`]).
    ColdStart(ColdStart),
    /// Cold with no such reason — one observation of the prefix having moved.
    /// Individually noisy; the streak is the signal.
    Cold,
}

/// The consecutive-unexplained-cold-read counter over main-loop requests
/// (docs/log-and-projection.md). The pins in `project.rs`/`api/test.rs` prove the
/// harness emits the bytes it emitted yesterday; only a read rate proves the
/// *provider* still matches them, so this folds the same `usage.jsonl` signal the
/// `cache` verb reads offline — live, as the daemon records each request.
///
/// Only `RequestOrigin::Turn` moves the streak: the side channels carry their own
/// prefixes at their own lineages. `Peek`, `Keepalive`, and `AbortedAttempt` ride
/// the main-loop prefix without being main-loop *evidence* — they stamp the touch
/// clock (an idle gap is measured from the last thing that could have refreshed
/// the prefix) but never count as cold. An aborted attempt in particular carries
/// all-zero usage by construction (docs/transport.md), so a zero `cache_read`
/// there says nothing about whether the prefix moved.
#[derive(Default)]
pub struct ColdStreak {
    streak: u32,
    last_touch_ms: Option<u128>,
    last_model: Option<String>,
}

impl ColdStreak {
    /// Fold one recorded request in, returning its classification for a main-loop
    /// request and `None` for anything else. Takes the same four fields
    /// `usage.jsonl` persists, so the offline verb replays a recorded session
    /// through this exact logic.
    pub fn observe(
        &mut self,
        origin: RequestOrigin,
        at_ms: u128,
        model: Option<&str>,
        cache_read: u64,
    ) -> Option<CacheRead> {
        let touches_prefix = matches!(
            origin,
            RequestOrigin::Turn
                | RequestOrigin::Peek
                | RequestOrigin::Keepalive
                | RequestOrigin::AbortedAttempt
        );
        let previous_touch = self.last_touch_ms;
        if touches_prefix {
            self.last_touch_ms = Some(at_ms);
        }
        if origin != RequestOrigin::Turn {
            return None;
        }
        let previous_model = self.last_model.clone();
        if let Some(model) = model {
            self.last_model = Some(model.to_string());
        }
        if cache_read > 0 {
            self.streak = 0;
            return Some(CacheRead::Warm);
        }
        // A cold read with an explanation resets the streak like a warm one: the
        // streak counts prefix evidence, and these three carry none.
        let explanation = match (previous_touch, previous_model) {
            (None, _) => Some(ColdStart::Boot),
            (Some(_), Some(before)) if model.is_some_and(|m| m != before) => {
                Some(ColdStart::ModelChange)
            }
            (Some(touch), _) if at_ms.saturating_sub(touch) > CACHE_TTL.as_millis() => {
                Some(ColdStart::Idle)
            }
            _ => None,
        };
        match explanation {
            Some(cause) => {
                self.streak = 0;
                Some(CacheRead::ColdStart(cause))
            }
            None => {
                self.streak += 1;
                Some(CacheRead::Cold)
            }
        }
    }

    /// Consecutive unexplained cold main-loop reads as of the last observation.
    pub fn streak(&self) -> u32 {
        self.streak
    }
}

/// One line of `usage.jsonl`: when, which shape, the upstream's own id for the
/// request, the model that served it, and the four token counts. The model is
/// per line rather than per file because the session's model can change
/// mid-log, so only a per-request record attributes every entry.
///
/// Readable as well as writable: the `cache` verb (docs/operating.md) replays a
/// recorded session's lines back through [`ColdStreak`], so the offline report and
/// the live counter share one classification. Missing counters read as zero, which
/// is what a line written before a field existed means.
#[derive(Serialize, Deserialize)]
pub struct UsageLine {
    pub at_ms: u128,
    pub origin: RequestOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
}

/// Now, as Unix millis — the stamp on a usage line and the input to the
/// idle-eviction test. Falls back to 0 on a clock before the epoch, as elsewhere.
fn wall_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

impl Shared {
    /// Open (append) the per-request usage record at `path`. Not opening one — tests,
    /// offline verbs — just drops the lines; a failure to open is reported and
    /// likewise degrades to in-memory telemetry only, never a dead daemon.
    pub fn attach_usage_log(&self, path: &std::path::Path) {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(file) => *self.usage_log.lock().unwrap() = Some(io::BufWriter::new(file)),
            Err(e) => eprintln!("[usage record: could not open {}: {e}]", path.display()),
        }
    }

    /// Append one origin-tagged line to `usage.jsonl` (flushed per line — each is a
    /// completed upstream request worth surviving a crash). Failures are reported and
    /// dropped: the record is diagnostic, never load-bearing.
    fn persist_usage(&self, origin: RequestOrigin, at_ms: u128, request: &RequestUsage) {
        use std::io::Write;
        let mut sink = self.usage_log.lock().unwrap();
        let Some(sink) = sink.as_mut() else { return };
        let line = UsageLine {
            at_ms,
            origin,
            id: request.id.clone(),
            model: request.model.clone(),
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
    /// latest plus running totals and the absolute stamp (the eviction-prediction
    /// signal, docs/log-and-projection.md). Transport-only — no canonical-log append,
    /// no projection change.
    pub fn record_request(&self, origin: RequestOrigin, request: RequestUsage) {
        let at_ms = wall_millis();
        self.persist_usage(origin, at_ms, &request);
        // An aborted attempt is all-zero usage by construction (docs/transport.md):
        // nothing to price, and fetching it would only run the generation lookup
        // to exhaustion and misreport it as `unpriced`.
        if origin != RequestOrigin::AbortedAttempt {
            self.spawn_cost_fetch(&request);
        }
        let mut m = self.metrics.lock().unwrap();
        if origin == RequestOrigin::Turn {
            m.last_turn_usage = Some(request.usage.clone());
        }
        m.cold_streak.observe(
            origin,
            at_ms,
            request.model.as_deref(),
            request.usage.cache_read_input_tokens,
        );
        Self::fold_usage(&mut m, request.usage);
        m.last_real_turn_at = Some(Instant::now());
    }

    /// A keepalive ping's usage (docs/log-and-projection.md). Persisted and folded into
    /// the telemetry and the cache-touch clock like any request, but it does **not**
    /// stamp the real-turn clock — a ping must not reset the cap that decides when to
    /// stop pinging.
    pub fn record_keepalive(&self, request: RequestUsage) {
        let at_ms = wall_millis();
        self.persist_usage(RequestOrigin::Keepalive, at_ms, &request);
        self.spawn_cost_fetch(&request);
        let mut m = self.metrics.lock().unwrap();
        // A ping refreshes the prefix it read, so it stamps the touch clock the
        // idle-eviction test reads — without ever counting as a main-loop request.
        m.cold_streak.observe(
            RequestOrigin::Keepalive,
            at_ms,
            request.model.as_deref(),
            request.usage.cache_read_input_tokens,
        );
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
        m.last_request_wall = Some(crate::log::Timestamp::now());
        m.last_usage = Some(usage);
    }

    /// Consecutive unexplained cold main-loop reads (docs/log-and-projection.md) —
    /// what `GET /state` publishes as `usage.cold_streak` and the TUI sidebar warns
    /// on past [`COLD_STREAK_WARN`].
    pub fn cold_streak(&self) -> u32 {
        self.metrics.lock().unwrap().cold_streak.streak()
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
