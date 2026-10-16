//! The keepalive's session-side state (docs/transport.md): each lineage's touch and
//! real-turn clocks, the primer's stored turns, and each loop's stance. The loop that
//! reads them is in `keepalive.rs`.

use super::*;

impl Session {
    /// `(since the lineage's last cache-touch, since the last real turn)` — the keepalive
    /// loop's two clocks. The first decides *when* to ping (the cache is nearing its TTL);
    /// the second enforces the *cap* (a ping doesn't reset it, so unbounded idle still
    /// stops). Both `None` until the first real turn; the first is also `None` on the
    /// triage lineage until a request on it has been recorded.
    ///
    /// A compaction since the lineage's last touch empties the first: the prefix that request
    /// read is gone, and nothing has sent the new one, so there is no entry to keep.
    pub(in crate::daemon) fn keepalive_clocks(
        &self,
        lineage: CacheLineage,
    ) -> (Option<Duration>, Option<Duration>) {
        let compacted_at = self.last_compaction_at();
        let m = self.metrics.lock().unwrap();
        let (wall, at) = match lineage {
            CacheLineage::Main => (m.last_request_wall, m.last_request_at),
            CacheLineage::Triage => (m.last_triage_wall, m.last_triage_at),
        };
        let touched =
            wall.filter(|touch| compacted_at.is_none_or(|compaction| touch.0 > compaction.0));
        (
            touched.and(at).map(|t| t.elapsed()),
            m.last_real_turn_at.map(|t| t.elapsed()),
        )
    }

    /// The prompt tokens the next ping on `lineage` would read: the main loop's last turn,
    /// or the primed turns' prompt on the triage lineage. `None` when there is nothing to
    /// ping, which also skips the affordability check.
    pub(in crate::daemon) fn ping_prefix_tokens(&self, lineage: CacheLineage) -> Option<u64> {
        match lineage {
            CacheLineage::Main => {
                let metrics = self.metrics.lock().unwrap();
                metrics.last_turn_usage.as_ref().map(prefix_tokens)
            }
            CacheLineage::Triage => self.with_triage_primed(|primed| primed.prefix_tokens),
        }
    }

    /// The turns one ping on `lineage` sends. The main loop's is the projection a real
    /// turn would send, lopped and closed with a breakpoint. The triage lineage's is the
    /// primed turns, byte for byte, so the ping is the primer's own write. `None` when no
    /// valid ping exists.
    pub(in crate::daemon) fn ping_turns(&self, lineage: CacheLineage) -> Option<Vec<WireTurn>> {
        match lineage {
            CacheLineage::Main => {
                let mut turns = self.project_turns();
                lop_trailing_model_turns(&mut turns);
                if turns.is_empty() {
                    return None;
                }
                turns.push(WireTurn::Breakpoint);
                Some(turns)
            }
            CacheLineage::Triage => self.with_triage_primed(|primed| primed.turns.clone()),
        }
    }

    /// Store the turns a primer just sent, for the triage keepalive to re-send
    /// (docs/transport.md). `sent` is taken before the send, and `usage` is the primer's
    /// own usage, whose prompt tokens are the prefix a ping reads back.
    pub(crate) fn set_triage_primed(
        &self,
        turns: &[WireTurn],
        sent: crate::log::Timestamp,
        usage: &RequestUsage,
    ) {
        self.metrics.lock().unwrap().triage_primed = Some(PrimedPrefix {
            turns: turns.to_vec(),
            sent,
            prefix_tokens: prefix_tokens(&usage.usage),
        });
    }

    /// Read the stored primer, provided no compaction has moved the prefix since it was sent. A
    /// stored primer that a compaction has passed is dropped here, so no ping replays bytes the
    /// log does not produce.
    fn with_triage_primed<T>(&self, read: impl FnOnce(&PrimedPrefix) -> T) -> Option<T> {
        let compacted_at = self.last_compaction_at();
        let mut m = self.metrics.lock().unwrap();
        let stale = match (&m.triage_primed, compacted_at) {
            (Some(primed), Some(compaction)) => primed.sent.0 <= compaction.0,
            _ => false,
        };
        if stale {
            m.triage_primed = None;
        }
        m.triage_primed.as_ref().map(read)
    }

    /// Publish a keepalive loop's stance (`usage.keepalive_skipped` for the main loop,
    /// `usage.triage_keepalive_skipped` for the triage lineage), or clear it. The stance
    /// is recomputed every tick, so an unchanged one keeps its original `at` and wakes
    /// nobody; the stderr line marks only the transitions into and out of skipping.
    pub(super) fn set_keepalive_skipped(&self, lineage: CacheLineage, skip: Option<KeepaliveSkip>) {
        let mut metrics = self.metrics.lock().unwrap();
        let current = match lineage {
            CacheLineage::Main => &mut metrics.keepalive_skipped,
            CacheLineage::Triage => &mut metrics.triage_keepalive_skipped,
        };
        if current.as_ref().map(|c| &c.why) == skip.as_ref().map(|s| &s.why) {
            return;
        }
        let name = lineage.name();
        match (&skip, current.is_some()) {
            (
                Some(KeepaliveSkip {
                    why:
                        KeepaliveSkipReason::Discard {
                            output_tokens,
                            prefix_tokens,
                            ..
                        },
                    ..
                }),
                false,
            ) => log::debug!(
                "{name} skipping: the last ping generated {output_tokens} output tokens, \
                 more than a refresh of a {prefix_tokens}-token prefix repays"
            ),
            (None, true) => log::debug!("{name} resumed: the prefix now repays a ping"),
            _ => {}
        }
        *current = skip;
        drop(metrics);
        self.touch_state();
    }
}
