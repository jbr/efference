//! The cache keepalive timer (docs/log-and-projection.md).

use super::*;

/// How long the cached prefix may go untouched before the keepalive pings it,
/// absent a `[transport] keepalive-interval` (docs/transport.md). A default
/// rather than a constant because the TTL is a provider fact the harness reads
/// nowhere.
pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(240);

/// The cache keepalive (docs/transport.md): fire a `max_tokens: 0` ping when the
/// canonical prefix nears eviction during idle, so the next real turn resumes hot.
/// Pure transport — it reads the prefix and records the usage; the log is never
/// touched.
///
/// Two clocks ([`Shared::keepalive_clocks`]): ping off the cache-touch clock, give
/// up off the *real-turn* clock, so a ping can never reset its own deadline.
pub(crate) async fn keepalive_loop(
    client: Arc<Client>,
    shared: Arc<Shared>,
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
        let mut messages = shared.project_messages();
        place_trailing_cache_breakpoint(&mut messages, client.breakpoint_placement());
        match client
            .keepalive(&messages, &tools, ToolChoice::auto())
            .await
        {
            Ok(request) => {
                // The read-only tripwire (docs/transport.md): `max_tokens: 0` is
                // supposed to generate nothing, but a mandatory-reasoning endpoint
                // reasons anyway and bills it as output. One offense ends the
                // keepalive for the session — a runtime check rather than a
                // provider gate, so it also catches an endpoint that stops
                // honoring the contract.
                let generated = request.usage.output_tokens;
                let cache_read = request.usage.cache_read_input_tokens;
                shared.record_keepalive(request);
                if generated > 0 {
                    eprintln!(
                        "[keepalive generated {generated} output tokens on a max_tokens:0 \
                         ping — not a read-only refresh on this endpoint; keepalive off \
                         for this session]"
                    );
                    return;
                }
                eprintln!("[keepalive: cache read {cache_read} — TTL refreshed]");
            }
            Err(e) => eprintln!("[keepalive failed: {e}]"),
        }
    }
}
