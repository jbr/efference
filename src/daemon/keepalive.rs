//! The cache keepalive timer (docs/log-and-projection.md).

use super::*;

/// How long the cached prefix may go untouched before the keepalive pings it,
/// absent a `[transport] keepalive-interval` (docs/transport.md): four fifths of the
/// provider's cache lifetime, so a ping lands with a margin before the entry it
/// refreshes would expire. Four minutes against a five-minute cache.
pub fn keepalive_interval(cache_ttl: Duration) -> Duration {
    cache_ttl * 4 / 5
}

/// How many prefix tokens must stand behind each token the ping discards for the
/// ping to still be worth sending (docs/transport.md). A `max_tokens: 0` ping is
/// supposed to generate nothing, but a mandatory-reasoning endpoint reasons anyway
/// and bills it as output, so the question is a cost comparison: the ping buys a
/// cache read that scales with the prefix and pays for whatever output it
/// generated.
///
/// Derivation: an output token costs about 50× a cache-read token on Anthropic,
/// and the factor drifts roughly 25–100× across providers; 100 is that 50 with a
/// margin factor of 2, which covers the drift. Order of magnitude is the intended
/// precision — the decision separates a ping worth ~cents from one worth dollars,
/// and exact prices are only available retrospectively (and not at all on
/// subscription endpoints).
const PREFIX_TOKENS_PER_DISCARDED_TOKEN: u64 = 100;

/// The cache keepalive (docs/transport.md): fire a `max_tokens: 0` ping when the
/// canonical prefix nears eviction during idle, so the next real turn resumes hot.
/// Pure transport — it reads the prefix and records the usage; the log is never
/// touched.
///
/// Two clocks ([`Session::keepalive_clocks`]): ping off the cache-touch clock, give
/// up off the *real-turn* clock, so a ping can never reset its own deadline.
pub(crate) async fn keepalive_loop(
    client: Arc<Client>,
    shared: Arc<Session>,
    tools: Vec<Value>,
    ping_at: Duration,
) {
    const CAP: Duration = Duration::from_secs(30 * 60); // give up after 30 min idle
    // Poll often enough that the ping lands near the interval rather than up to a
    // full check-period late — a quarter of the interval, capped at 20s. The floor
    // keeps a very short interval from becoming a busy loop.
    let check = (ping_at / 4).clamp(Duration::from_secs(1), Duration::from_secs(20));
    loop {
        tokio::time::sleep(check).await;
        // Need a real turn to have happened (a prefix worth keeping warm) and to be
        // inside the window: nearing eviction but not yet past the give-up cap.
        let (Some(touch), Some(turn)) = shared.keepalive_clocks() else {
            continue;
        };
        if turn >= CAP || touch < ping_at {
            continue;
        }
        // Same prefix a real turn would send (byte-identical, or it writes a new
        // entry instead of refreshing the live one); `max_tokens: 0` reads it.
        let mut turns = shared.project_turns();
        lop_trailing_model_turns(&mut turns);
        if turns.is_empty() {
            // Nothing but model turns in the log (or nothing at all): there is no
            // valid ping to send, and no prefix a real turn would reuse either.
            continue;
        }
        turns.push(WireTurn::Breakpoint);
        match client.keepalive(&turns, &tools, ToolChoice::auto()).await {
            Ok(request) => {
                // The read-only tripwire (docs/transport.md): `max_tokens: 0` is
                // supposed to generate nothing, but a mandatory-reasoning endpoint
                // reasons anyway and bills it as output. Tolerated while the prefix
                // the ping keeps warm is large enough to dominate the discarded
                // output; past that the ping costs more than the cold read it
                // avoids, and keepalive ends for the session. A runtime check
                // rather than a provider gate, so it also catches an endpoint that
                // stops honoring the contract.
                let discarded = request.usage.output_tokens;
                let prefix = prefix_tokens(&request.usage);
                let cache_read = request.usage.cache_read_input_tokens;
                shared.record_keepalive(request);
                if !discard_is_affordable(discarded, prefix) {
                    eprintln!(
                        "[keepalive generated {discarded} output tokens on a max_tokens:0 \
                         ping against a {prefix}-token prefix — too much discarded output \
                         to be worth the refresh; keepalive off for this session]"
                    );
                    return;
                }
                eprintln!("[keepalive: cache read {cache_read} — TTL refreshed]");
            }
            // A rejected *request* is rejected identically next tick, so retrying it
            // at the check cadence only reprints the same line for the life of the
            // pause. Terminal for the session, same shape as the output tripwire;
            // transient failures (5xx, network, timeouts, rate limits) keep retrying.
            Err(e) if e.is_permanent() => {
                eprintln!(
                    "[keepalive rejected by the upstream: {e}; keepalive off for this session]"
                );
                return;
            }
            Err(e) => eprintln!("[keepalive failed: {e}]"),
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
/// prefix was still worth sending, per [`PREFIX_TOKENS_PER_DISCARDED_TOKEN`]. The
/// ratio self-scales: a large prefix absorbs a few thousand reasoning tokens, a
/// small one absorbs none, and a runaway generation fails it at any prefix size.
/// A ping that generated nothing is affordable whatever the reported prefix,
/// including an endpoint that reports no usage at all.
pub(super) fn discard_is_affordable(discarded: u64, prefix: u64) -> bool {
    discarded == 0 || discarded.saturating_mul(PREFIX_TOKENS_PER_DISCARDED_TOKEN) < prefix
}
