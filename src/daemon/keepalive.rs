//! The cache keepalive timer (docs/log-and-projection.md).

mod session;

use super::*;
use crate::api::TokenPrices;
use crate::triage::Triager;

/// How long the cached prefix may go untouched before the keepalive pings it,
/// absent a `[transport] keepalive-interval` (docs/transport.md): four fifths of the
/// provider's cache lifetime, so a ping lands with a margin before the entry it
/// refreshes would expire. Four minutes against a five-minute cache.
pub fn keepalive_interval(cache_ttl: Duration) -> Duration {
    cache_ttl * 4 / 5
}

/// The cache lineage a keepalive loop keeps warm (docs/transport.md): the main prefix the
/// agent's turns write, or the triage lineage a separate triage model's primer plants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheLineage {
    Main,
    Triage,
}

impl CacheLineage {
    /// The origin the lineage's pings are tagged with (`usage.jsonl`, `dev cache`).
    fn origin(self) -> RequestOrigin {
        match self {
            Self::Main => RequestOrigin::Keepalive,
            Self::Triage => RequestOrigin::TriageKeepalive,
        }
    }

    /// The name the loop's stderr lines and the stance's wording use.
    fn name(self) -> &'static str {
        match self {
            Self::Main => "keepalive",
            Self::Triage => "triage keepalive",
        }
    }
}

/// One keepalive loop's ping: the lineage it keeps warm, the envelope it sends, and its
/// cadence (docs/transport.md). The session runtime builds one per armed lineage.
pub(crate) struct KeepaliveSpec {
    pub(crate) lineage: CacheLineage,
    pub(crate) client: Arc<Client>,
    pub(crate) tools: Vec<Value>,
    pub(crate) choice: ToolChoice,
    pub(crate) ping_at: Duration,
    pub(crate) prices: Option<TokenPrices>,
}

impl KeepaliveSpec {
    /// The triage lineage's keepalive (docs/transport.md): the primer's own envelope
    /// re-sent, armed only where the primer can plant (a shared triage has none, and
    /// neither does a Messages-dialect triage model). `configured` is
    /// `[transport] keepalive-interval`, which applies to both lineages; absent, the
    /// cadence is the triage provider's cache lifetime, as the main loop's is.
    pub(crate) fn triage(
        peek: &Triager,
        configured: Option<Duration>,
        prices: Option<TokenPrices>,
    ) -> Option<Self> {
        let (client, tools, choice) = peek.primer_envelope()?;
        let ping_at = configured
            .unwrap_or_else(|| keepalive_interval(client.endpoint().provider.cache_ttl()));
        Some(Self {
            lineage: CacheLineage::Triage,
            client: Arc::new(client),
            tools,
            choice,
            ping_at,
            prices,
        })
    }
}

/// The turns the last primer sent (docs/transport.md), which the triage keepalive re-sends
/// byte for byte.
#[derive(Clone)]
pub(super) struct PrimedPrefix {
    /// The primer's input, as sent.
    pub(super) turns: Vec<WireTurn>,
    /// When the primer was sent, taken before the send: a fold stamped after it moved the
    /// prefix the primer read, so the stored turns stop describing the live entry.
    pub(super) sent: crate::log::Timestamp,
    /// The prompt tokens the primer read: what a ping of these turns reads back.
    pub(super) prefix_tokens: u64,
}

/// From when the session's cache is believed to have expired (backlog: cold-session.md):
/// the last request on the main prefix plus the provider's cache lifetime, once `now`
/// is past it. `None` while it is believed warm, and for a session with no such request
/// yet, which has nothing cached to lose. A belief, not a measurement: no provider says
/// whether an entry is still held, so this errs toward cold.
pub fn believed_cold_since(
    last_touch: Option<crate::log::Timestamp>,
    now: crate::log::Timestamp,
    cache_ttl: Duration,
) -> Option<crate::log::Timestamp> {
    let ttl_ms = u64::try_from(cache_ttl.as_millis()).unwrap_or(u64::MAX);
    let expires = crate::log::Timestamp(last_touch?.0.saturating_add(ttl_ms));
    (now.0 > expires.0).then_some(expires)
}

/// How many prefix tokens must stand behind each token the ping discards for the
/// ping to still be worth sending, when no prices are known (docs/transport.md). A
/// zero-output ping is supposed to generate nothing, but a mandatory-reasoning
/// endpoint reasons anyway and bills it as output, so the question is a cost
/// comparison: the ping buys a cache read that scales with the prefix and pays for
/// whatever output it generated.
///
/// Derivation: an output token costs about 50× a cache-read token on Anthropic,
/// and the factor drifts roughly 25–100× across providers; 100 is that 50 with a
/// margin factor of 2, which covers the drift. Order of magnitude is the intended
/// precision — the decision separates a ping worth ~cents from one worth dollars.
/// The ratio compares output with the *cache-read* price, so it over-weighs output
/// wherever the cache-read price sits far below the output price's usual 50×
/// multiple; [`PREMIUM_MARGIN`] is the rule for sessions that know their prices.
const PREFIX_TOKENS_PER_DISCARDED_TOKEN: u64 = 100;

/// How many times the ping's completion cost must fit in the cold-read premium it
/// avoids, when prices are known (docs/transport.md). The premium is what the next
/// real turn would pay extra if the entry had expired; the ping is sent at four
/// fifths of the cache lifetime, before anyone knows the session would have idled
/// past it, so the saving is only realized in the cases where the cache would have
/// lapsed. Requiring the cost to fit in half the premium keeps a ping that is
/// right about as often as it is wrong from being a net loss.
const PREMIUM_MARGIN: f64 = 2.0;

/// What the keepalive is doing other than pinging normally, published as
/// `usage.keepalive_skipped` (docs/control-api.md). Absent from the state while pings
/// go out on schedule, or when the keepalive was never armed (no prompt cache,
/// interval zero): it records a decision the loop made, not a configuration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct KeepaliveSkip {
    /// When the loop reached this stance, absolute Unix millis.
    pub(crate) at: crate::log::Timestamp,
    #[serde(flatten)]
    pub(crate) why: KeepaliveSkipReason,
}

/// The two reasons the loop does not ping.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub(crate) enum KeepaliveSkipReason {
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

/// What the most recent ping discarded: the estimate of what the next one will, on
/// this endpoint. The loop's only memory between ticks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PingCost {
    pub(super) output_tokens: u64,
    /// The completion cost in USD where the receipt or the prices give one.
    pub(super) completion_usd: Option<f64>,
}

impl PingCost {
    pub(super) fn of(request: &RequestUsage, prices: Option<&TokenPrices>) -> Self {
        Self {
            output_tokens: request.usage.output_tokens,
            completion_usd: completion_cost(request, prices),
        }
    }
}

/// The cache keepalive (docs/transport.md): fire a ping (capped at
/// [`crate::api::Provider::keepalive_output_cap`]) when the lineage's cached prefix nears
/// eviction during idle, so the next real turn resumes hot. Pure transport — it reads the
/// prefix and records the usage; the log is never touched.
///
/// One loop per armed lineage ([`CacheLineage`]). Each reads its own clock, stance, and
/// prefix; the give-up cap reads the real-turn clock, which both share.
///
/// Two clocks ([`Session::keepalive_clocks`]): ping off the lineage's cache-touch clock,
/// give up off the *real-turn* clock, so a ping can never reset its own deadline.
pub(crate) async fn keepalive_loop(spec: KeepaliveSpec, shared: Arc<Session>) {
    const CAP: Duration = Duration::from_secs(30 * 60); // give up after 30 min idle
    let KeepaliveSpec {
        lineage,
        client,
        tools,
        choice,
        ping_at,
        prices,
    } = spec;
    let name = lineage.name();
    // Poll often enough that the ping lands near the interval rather than up to a
    // full check-period late — a quarter of the interval, capped at 20s. The floor
    // keeps a very short interval from becoming a busy loop.
    let check = (ping_at / 4).clamp(Duration::from_secs(1), Duration::from_secs(20));
    // What the last ping discarded; none before the first ping, which is sent
    // unjudged.
    let mut estimate: Option<PingCost> = None;
    // The real-turn clock's reading when a ping met an exhausted limit. Pinging
    // stays off while that clock keeps running from the same turn; a new real turn
    // restarts it below this reading, and the pings resume.
    let mut exhausted_at: Option<Duration> = None;
    loop {
        tokio::time::sleep(check).await;
        // Need a real turn to have happened (a prefix worth keeping warm) and to be
        // inside the window: nearing eviction but not yet past the give-up cap.
        let (Some(touch), Some(turn)) = shared.keepalive_clocks(lineage) else {
            continue;
        };
        if exhausted_at.is_some_and(|at| turn >= at) {
            continue;
        }
        exhausted_at = None;
        if turn >= CAP || touch < ping_at {
            continue;
        }
        // The affordability check (docs/transport.md), per tick and before sending:
        // the last ping's discard against the prefix as it stands now. A skipped
        // tick sends nothing, so the next one re-evaluates and a prefix that has
        // grown past break-even resumes the pings.
        if let (Some(estimate), Some(prefix)) = (estimate, shared.ping_prefix_tokens(lineage))
            && let Some(why) = skip_reason(estimate, prefix, prices.as_ref())
        {
            shared.set_keepalive_skipped(
                lineage,
                Some(KeepaliveSkip {
                    at: crate::log::Timestamp::now(),
                    why,
                }),
            );
            continue;
        }
        shared.set_keepalive_skipped(lineage, None);
        // Same prefix a real turn would send (byte-identical, or it writes a new
        // entry instead of refreshing the live one); the output cap keeps it a read.
        // Nothing to ping (the log holds only model turns, or no primer has been sent
        // on the triage lineage) leaves no prefix to keep.
        let Some(turns) = shared.ping_turns(lineage) else {
            continue;
        };
        match client
            .keepalive(lineage.origin(), &turns, &tools, choice)
            .await
        {
            Ok(request) => {
                let cache_read = request.usage.cache_read_input_tokens;
                estimate = Some(PingCost::of(&request, prices.as_ref()));
                match lineage {
                    CacheLineage::Main => shared.record_keepalive(request),
                    CacheLineage::Triage => shared.record_triage_keepalive(request),
                }
                log::debug!("{name}: cache read {cache_read} — TTL refreshed");
            }
            // A rejected *request* is rejected identically next tick, so retrying it
            // at the check cadence only reprints the same line for the life of the
            // pause. Terminal for the session; transient failures (5xx, network,
            // timeouts, rate limits) keep retrying.
            Err(e) if e.is_permanent() => {
                log::debug!("{name} rejected by the upstream: {e}; {name} off for this session");
                shared.set_keepalive_skipped(
                    lineage,
                    Some(KeepaliveSkip {
                        at: crate::log::Timestamp::now(),
                        why: KeepaliveSkipReason::Rejected {
                            detail: e.to_string(),
                        },
                    }),
                );
                return;
            }
            // A limit that holds until it resets (a plan's usage limit, which Codex
            // sends as a 429) answers every ping identically until then; one line,
            // and no pings until the next real turn.
            Err(e) if e.is_exhausted() => {
                log::debug!("{name} paused until the next turn: {e}");
                shared.set_keepalive_skipped(
                    lineage,
                    Some(KeepaliveSkip {
                        at: crate::log::Timestamp::now(),
                        why: KeepaliveSkipReason::Exhausted {
                            detail: e.to_string(),
                            resets_at: e.resets_at(),
                        },
                    }),
                );
                exhausted_at = Some(turn);
            }
            Err(e) => log::debug!("{name} failed: {e}"),
        }
    }
}

/// Drop trailing assistant messages so the ping ends on a user-role message
/// (docs/transport.md). Tool results project as user role, so this removes exactly
/// the model output that has never been sent as input: a settled session's closing
/// prose, or the `tool_use` turn a pending approval gate is parked on. Some
/// providers reject a request that ends with a model turn outright, and the lopped
/// prefix is the one the last request already cached, so the ping is still a read
/// of the live entry.
///
/// Transport-side only: the projection itself is untouched, and its byte-stability
/// pins do not move.
pub(super) fn lop_trailing_model_turns(turns: &mut Vec<WireTurn>) {
    while turns
        .last()
        .is_some_and(|t| matches!(t, WireTurn::Assistant(_) | WireTurn::Breakpoint))
    {
        turns.pop();
    }
}

/// The prompt tokens the ping read, however the endpoint accounted for them —
/// fresh input, cache read, and cache write are all "prefix this ping paid to
/// send", and providers split the same prefix across these three fields
/// differently.
pub(super) fn prefix_tokens(usage: &Usage) -> u64 {
    usage
        .input_tokens
        .saturating_add(usage.cache_read_input_tokens)
        .saturating_add(usage.cache_creation_input_tokens)
}

/// Whether a ping that discarded `discarded` output tokens over a `prefix`-token
/// prefix was still worth sending, when no prices are known, per
/// [`PREFIX_TOKENS_PER_DISCARDED_TOKEN`]. The ratio self-scales: a large prefix
/// absorbs a few thousand reasoning tokens, a small one absorbs none, and a runaway
/// generation fails it at any prefix size. A ping that generated nothing is
/// affordable whatever the reported prefix, including an endpoint that reports no
/// usage at all.
pub(super) fn discard_is_affordable(discarded: u64, prefix: u64) -> bool {
    discarded == 0 || discarded.saturating_mul(PREFIX_TOKENS_PER_DISCARDED_TOKEN) < prefix
}

/// The ping's completion cost in USD: the endpoint's inline `completion_usd` when it
/// billed one, else the discarded tokens at the session's completion price. `None`
/// without either.
pub(super) fn completion_cost(request: &RequestUsage, prices: Option<&TokenPrices>) -> Option<f64> {
    request
        .receipt
        .cost
        .and_then(|cost| cost.completion_usd)
        .or_else(|| prices.map(|p| request.usage.output_tokens as f64 * p.completion))
}

/// Why the next ping is not worth sending, or `None` when it is (docs/transport.md).
/// `estimate` is what the last ping discarded and `prefix` the prompt tokens the next
/// would read. With prices, the cost of the output must fit within `1 /
/// PREMIUM_MARGIN` of what a cold read of the prefix would cost beyond a warm one:
/// `completion cost < prefix × (prompt − cache read) / PREMIUM_MARGIN`. Without prices
/// — Anthropic direct, custom endpoints, an OpenRouter model whose endpoints list no
/// cache-read price — [`discard_is_affordable`] decides on token counts alone. A ping
/// that generated nothing is affordable on either rule.
pub(super) fn skip_reason(
    estimate: PingCost,
    prefix: u64,
    prices: Option<&TokenPrices>,
) -> Option<KeepaliveSkipReason> {
    let premium_usd = prices.map(|p| prefix as f64 * p.premium());
    let affordable = match (prices, premium_usd) {
        (Some(_), Some(premium)) => {
            estimate.output_tokens == 0
                || estimate
                    .completion_usd
                    .is_some_and(|cost| cost < premium / PREMIUM_MARGIN)
        }
        _ => discard_is_affordable(estimate.output_tokens, prefix),
    };
    (!affordable).then_some(KeepaliveSkipReason::Discard {
        output_tokens: estimate.output_tokens,
        completion_usd: estimate.completion_usd,
        prefix_tokens: prefix,
        premium_usd,
    })
}
