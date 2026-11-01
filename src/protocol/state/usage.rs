//! The session's token, cache and cost telemetry (docs/control-api.md, docs/transport.md):
//! the `usage` topic and the `cold` topic. The host folds it from each request it records
//! (`session/metrics.rs`) and from the keepalive's decisions (`session/keepalive.rs`).

use serde::{Deserialize, Serialize};

use crate::inference::api::{CacheMissReason, PlanSnapshot, Usage};
use crate::record::log::Timestamp;
use crate::record::project::Retention;

/// The `usage` topic: token counts, the prompt cache's health, the session's cost, and
/// the Codex backend's plan windows, for the main loop and for a triage client of its own.
///
/// Every instant is absolute Unix millis, so the topic stays quiet until a request
/// happens: a client subtracts from its own clock (docs/control-api.md).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct UsageView {
    /// The last request's token counts, whatever it was for; `null` before the first.
    pub last: Option<Usage>,
    /// The counts summed over every request the session has made.
    pub cumulative: Usage,
    /// The last request on the main prefix: a turn, a keepalive ping, or a triage on the
    /// agent's own client. Persists across a host restart; `null` before the first.
    pub last_request_at: Option<Timestamp>,
    /// The boot-fetched context window and the last turn's prompt against it; `null`
    /// when no capabilities were fetched.
    pub context: Option<ContextWindow>,
    /// The billed session total, where the provider has a per-request accounting
    /// surface; `null` where it has none.
    pub cost: Option<Cost>,
    /// Consecutive main-loop requests that read nothing from the cache and had no
    /// legitimate reason to (docs/log-and-projection.md); 0 once a turn reads warm. A
    /// number rather than a flag, so the threshold is the client's ([`COLD_STREAK_WARN`]).
    pub cold_streak: u32,
    /// The same count over a separate triage client's reads; `null` while the triage
    /// borrows the agent's client.
    pub triage_cold_streak: Option<u32>,
    /// What the provider said about the most recent miss it attributed; `null` until one
    /// arrives.
    pub last_cache_miss: Option<LastCacheMiss>,
    /// The main keepalive's stance while it is not pinging on schedule; `null` while it
    /// is, and when none is armed.
    pub keepalive_skipped: Option<KeepaliveSkip>,
    /// The triage lineage's keepalive stance, by the same rule.
    pub triage_keepalive_skipped: Option<KeepaliveSkip>,
    /// The Codex backend's subscription windows as of the last response that named them;
    /// `null` on every other provider.
    pub plan: Option<PlanSnapshot>,
    /// A separate triage client's own windows, in the same shape; `null` while the triage
    /// borrows the agent's client, and when its provider reports none.
    pub triage_plan: Option<PlanSnapshot>,
    /// How much of what the triage was shown survives on the wire (docs/triage.md),
    /// folded from the log.
    pub retention: Retention,
}

/// The model's context window and how much of it the last turn's prompt used
/// (docs/transport.md).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ContextWindow {
    /// The window, as the provider reported it at boot; `null` when it reported none.
    pub limit: Option<u64>,
    /// The last main-loop request's prompt tokens — uncached input, cache reads and cache
    /// writes together; `null` before the first turn.
    pub used: Option<u64>,
}

/// The session's billed cost, where the provider prices requests (docs/transport.md).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Cost {
    pub usd: f64,
    /// Requests that could not be priced, which make `usd` an undercount.
    pub unpriced: u32,
}

/// The most recent cache miss the provider attributed, as `GET /state` publishes it
/// (docs/control-api.md). The reason is the provider's own spelling, and two of the six
/// values — `previous_message_not_found` and `unavailable` — mean no comparison was
/// produced rather than naming a divergence, so a client that renders this as a
/// *cause* has to say so for those two.
///
/// `at` is absolute wall-clock millis, like `last_request_at` and for the same reason
/// (docs/control-api.md): an elapsed-time field changes on every read, which under
/// level-triggered emission means the topic never goes quiet.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct LastCacheMiss {
    pub reason: CacheMissReason,
    /// The provider's byte-derived estimate of the prefix that fell after the
    /// divergence. Absent on the reasons that carry none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_missed_input_tokens: Option<u64>,
    pub at: Timestamp,
}

/// What the keepalive is doing other than pinging normally, published as
/// `usage.keepalive_skipped` (docs/control-api.md). Absent from the state while pings
/// go out on schedule, or when the keepalive was never armed (no prompt cache,
/// interval zero): it records a decision the loop made, not a configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct KeepaliveSkip {
    /// When the loop reached this stance, absolute Unix millis.
    pub at: Timestamp,
    #[serde(flatten)]
    pub why: KeepaliveSkipReason,
}

/// The two reasons the loop does not ping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum KeepaliveSkipReason {
    /// The last ping discarded output that the refresh would not repay against the
    /// current prefix (docs/transport.md). Re-evaluated every tick: the stance ends
    /// when the prefix grows past break-even.
    Discard {
        /// Output tokens the last ping generated.
        output_tokens: u64,
        /// That ping's completion cost in USD, when the endpoint billed it inline or
        /// the session's prices could compute it; `null` on the token-ratio rule.
        completion_usd: Option<f64>,
        /// The prompt tokens the next ping would read: the main prefix as it stands.
        prefix_tokens: u64,
        /// What a cold read of that prefix would cost beyond a warm one, in USD;
        /// `null` on the token-ratio rule.
        premium_usd: Option<f64>,
    },
    /// The upstream refused the ping request itself, which would be refused
    /// identically next tick. The one stance that does not end: the loop stops.
    Rejected {
        /// The upstream's refusal.
        detail: String,
    },
    /// The upstream answered with a limit that holds until it resets (a plan's usage
    /// limit). Pinging resumes after the next real turn, which is the first request
    /// that can learn the limit has passed.
    Exhausted {
        /// The upstream's refusal.
        detail: String,
        /// When the limit resets, epoch milliseconds, where the refusal says.
        resets_at: Option<u64>,
    },
}

/// The `cold` topic: the session's prompt cache is believed to have expired
/// (docs/control-api.md). The topic is `null` while the cache is believed warm, before
/// the first turn, on a model with no cache, and when there is nothing to compact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Cold {
    /// When the cache is believed to have expired: the last request on the main prefix
    /// plus the provider's cache lifetime.
    pub since: Timestamp,
    /// The last turn's prompt, in tokens: what reading the context cold would cost.
    /// `null` before a turn's usage is known.
    pub context_tokens: Option<u64>,
}

/// How many consecutive unexplained cold main-loop requests before the streak is
/// worth surfacing (docs/log-and-projection.md). Four, from a fold over this repo's
/// own corpus (77 `usage.jsonl` files, 44 with turns, 1371 `turn` requests):
/// 155 of those turns read nothing from the cache, and the host's classifier
/// (`session::ColdStreak`) explains 54 of them (38 session boot, 9 idle past the TTL, 7 a model
/// change). The remaining 101 fall into 28 runs — 24 of length 1, two of length 3,
/// and two of length 28 and 43, each of which was a whole session that never once
/// read the cache. So runs of 1–3 are what normal operation produces (a retry that
/// re-sent a prefix, an eviction this heuristic misses), and the defect signal is an
/// order of magnitude longer; 4 sits in the empty band between them.
pub const COLD_STREAK_WARN: u32 = 4;
