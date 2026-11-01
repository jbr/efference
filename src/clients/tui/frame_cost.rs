//! Frame-cost instrumentation (`--frame-cost`), off unless asked for.
//!
//! `render_cost.rs` pins the *structural* property — that a frame's work tracks the
//! viewport rather than the log — by counting nodes laid out. That count is stable
//! and testable, which is why it is what the tests assert on. It says nothing about
//! wall-clock, so it cannot answer whether a streaming render keeps up with the
//! 30 ms paint tick on a real terminal. This records the durations to go with the
//! counts.
//!
//! Deliberately not reachable from the snapshot harness: a rendered frame time is
//! not reproducible, so a snapshot containing one could never match twice.

use std::collections::VecDeque;

/// Frames kept for the live readout — about four seconds at the 30 ms tick, so the
/// figures move fast enough to attribute to what is on screen right now.
const RECENT_FRAMES: usize = 120;

/// The paint tick's period. A frame that takes longer than this is one the loop
/// could not deliver on time, which is the number that matters: it is the
/// difference between "the render is expensive" and "the render is behind".
const TICK_MICROS: u32 = 30_000;

/// Durations of recent paints, plus the node counts from the most recent one.
#[derive(Debug, Default)]
pub(super) struct FrameCost {
    recent: VecDeque<u32>,
    all: Vec<u32>,
    /// Projection time attributed to the *next* paint. Re-projection runs before
    /// the draw and can also run mid-burst on a key, so it is accumulated as it
    /// happens and folded in when the paint it fed lands. Counted separately
    /// because it is a different fix when it dominates: the projection rebuilds
    /// the live node from the whole accumulated buffer, where the paint's cost is
    /// laying out what that node became.
    pending_proj: u32,
    recent_proj: VecDeque<u32>,
    all_proj: Vec<u32>,
    last_laid_out: usize,
    last_highlighted: usize,
}

impl FrameCost {
    /// Attribute a re-projection to the paint it feeds. Several can land between
    /// two paints (a drained burst re-projects once, a key can force another), and
    /// what matters is the total a frame carried.
    pub(super) fn note_projection(&mut self, micros: u32) {
        self.pending_proj = self.pending_proj.saturating_add(micros);
    }

    /// Record one paint: how long it took, and what the walk did.
    pub(super) fn record(&mut self, micros: u32, laid_out: usize, highlighted: usize) {
        self.recent.push_back(micros);
        if self.recent.len() > RECENT_FRAMES {
            self.recent.pop_front();
        }
        self.all.push(micros);
        self.recent_proj.push_back(self.pending_proj);
        if self.recent_proj.len() > RECENT_FRAMES {
            self.recent_proj.pop_front();
        }
        self.all_proj.push(self.pending_proj);
        self.pending_proj = 0;
        self.last_laid_out = laid_out;
        self.last_highlighted = highlighted;
    }

    /// The status-bar readout over the recent window: median and worst frame time,
    /// and what the last walk actually did. `laid` is top-level nodes laid out and
    /// `hl` is lines syntax-highlighted — both should sit near zero in a steady
    /// stream, and a `hl` that tracks the block's total size is the memo missing.
    pub(super) fn readout(&self) -> Option<String> {
        let p50 = percentile(&self.recent, 50)?;
        let p95 = percentile(&self.recent, 95)?;
        let max = *self.recent.iter().max()?;
        let proj = percentile(&self.recent_proj, 50)?;
        Some(format!(
            "⟨{} p50 · {} p95 · {} max · proj {} · laid {} hl {}⟩",
            ms(p50),
            ms(p95),
            ms(max),
            ms(proj),
            self.last_laid_out,
            self.last_highlighted,
        ))
    }

    /// The whole session's distribution, printed to stderr after the alt-screen is
    /// restored. The over-tick count is the headline: frames slower than the tick
    /// are the ones the operator sees as the UI lagging the stream.
    pub(super) fn summary(&self) -> Option<String> {
        if self.all.is_empty() {
            return None;
        }
        let over = self.all.iter().filter(|&&d| d > TICK_MICROS).count();
        let sorted: VecDeque<u32> = {
            let mut v = self.all.clone();
            v.sort_unstable();
            v.into()
        };
        let proj: VecDeque<u32> = {
            let mut v = self.all_proj.clone();
            v.sort_unstable();
            v.into()
        };
        Some(format!(
            "frame cost over {} frames: p50 {} · p90 {} · p99 {} · max {} · \
             {over} over the {}ms tick ({:.1}%)\n\
             projection feeding those frames: p50 {} · p90 {} · p99 {} · max {}",
            self.all.len(),
            ms(percentile(&sorted, 50)?),
            ms(percentile(&sorted, 90)?),
            ms(percentile(&sorted, 99)?),
            ms(*sorted.back()?),
            TICK_MICROS / 1000,
            100.0 * over as f64 / self.all.len() as f64,
            ms(percentile(&proj, 50)?),
            ms(percentile(&proj, 90)?),
            ms(percentile(&proj, 99)?),
            ms(*proj.back()?),
        ))
    }
}

/// The `p`th percentile of `samples`, which need not be sorted — nearest-rank, on a
/// copy, because the caller's order is the arrival order it wants to keep.
fn percentile(samples: &VecDeque<u32>, p: usize) -> Option<u32> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted: Vec<u32> = samples.iter().copied().collect();
    sorted.sort_unstable();
    let rank = (p * sorted.len()).div_ceil(100).saturating_sub(1);
    sorted.get(rank).copied()
}

/// Microseconds as milliseconds, with one decimal below 10 ms — the interesting
/// range starts around a millisecond, where whole milliseconds would round to noise.
fn ms(micros: u32) -> String {
    let millis = micros as f64 / 1000.0;
    if millis < 10.0 {
        format!("{millis:.1}ms")
    } else {
        format!("{millis:.0}ms")
    }
}
