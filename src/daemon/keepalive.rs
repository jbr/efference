//! The cache keepalive timer (docs/log-and-projection.md).

use super::*;

/// The cache keepalive (docs/log-and-projection.md): a daemon-side timer that fires a
/// `max_tokens: 0` ping when the canonical prefix nears its 5-minute cache TTL
/// during idle, refreshing it so the next real turn resumes hot instead of cold.
/// Pure transport — it reads the prefix and records the usage; the log is never
/// touched.
///
/// Two clocks ([`Shared::keepalive_clocks`]): ping when the cache-touch clock
/// nears the TTL, but stop once the *real-turn* clock passes `CAP` — past that the
/// human has been gone long enough that pinging forever costs more than one cold
/// read on resume (the crossover, docs/log-and-projection.md). 5m TTL only: a warm cache can't be entered
/// at 1h (verified — TTL is write-time), so 1h is a future cold-write knob, not a
/// keepalive concern.
pub(crate) async fn keepalive_loop(client: Arc<Client>, shared: Arc<Shared>, tools: Vec<Value>) {
    const CHECK: Duration = Duration::from_secs(20);
    const PING_AT: Duration = Duration::from_secs(240); // 4 min — margin under the 5m TTL
    const CAP: Duration = Duration::from_secs(30 * 60); // give up after 30 min idle
    loop {
        tokio::time::sleep(CHECK).await;
        // Need a real turn to have happened (a prefix worth keeping warm) and to be
        // inside the window: nearing eviction but not yet past the give-up cap.
        let (Some(touch), Some(turn)) = shared.keepalive_clocks() else {
            continue;
        };
        if turn >= CAP || touch < PING_AT {
            continue;
        }
        // Same prefix a real turn would send (byte-identical, or it writes a new
        // entry instead of refreshing the live one); `max_tokens: 0` reads it.
        let mut messages = shared.project_messages();
        place_trailing_cache_breakpoint(&mut messages);
        match client
            .keepalive(&messages, &tools, ToolChoice::auto())
            .await
        {
            Ok(request) => {
                // The read-only tripwire: `max_tokens: 0` is supposed to
                // generate nothing, but a mandatory-reasoning endpoint reasons
                // anyway (observed live 2026-08-21: ~4k thinking tokens per
                // ping, billed as output). One offense ends the keepalive for
                // the session — a runtime observation, not a provider
                // assumption, so it also catches any future endpoint that
                // stops honoring the contract.
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
