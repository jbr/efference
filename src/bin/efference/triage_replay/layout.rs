//! Where the text view's cuts fall over a recorded session (docs/triage.md): a dry-run
//! report, and the per-triage line a live replay prints before its draws.

use super::Recorded;
use efference::log::LogEntry;
use efference::project::Reader;
use efference::triage::{
    Pending, TRIAGED_TAIL_HIGH_WATER, TRIAGED_TAIL_LOW_WATER, text_view_layout,
};

/// One line for the triage `g`: the history's sizes and the cuts made so far.
pub(super) fn describe(entries: &[LogEntry], g: &Recorded, reader: Reader) -> String {
    let pending: Vec<Pending<'_>> = g
        .calls
        .iter()
        .zip(&g.raws)
        .map(|(call, raw)| Pending { call, raw })
        .collect();
    let layout = text_view_layout(&entries[..g.prefix_end], &pending, reader);
    let last = match layout.cuts.last() {
        Some(seq) => format!(", the last at seq {seq}"),
        None => String::new(),
    };
    format!(
        "text view: digest {} · triaged tail {} · {} cut(s){last}",
        kib(layout.digest_bytes),
        kib(layout.tail_bytes),
        layout.cuts.len(),
    )
}

/// Print the layout at every selected triage, marking the triages where a cut fell.
pub(super) fn report(entries: &[LogEntry], selected: &[&Recorded], reader: Reader) {
    println!(
        "text view · triaged tail cut at {} back to {}",
        kib(TRIAGED_TAIL_HIGH_WATER),
        kib(TRIAGED_TAIL_LOW_WATER)
    );
    let mut seen = 0;
    for g in selected {
        let pending: Vec<Pending<'_>> = g
            .calls
            .iter()
            .zip(&g.raws)
            .map(|(call, raw)| Pending { call, raw })
            .collect();
        let layout = text_view_layout(&entries[..g.prefix_end], &pending, reader);
        let mark = match layout.cuts.len() > seen {
            true => format!(
                " · CUT: triaged tail now starts at seq {}",
                layout.cuts.last().copied().unwrap_or_default()
            ),
            false => String::new(),
        };
        seen = layout.cuts.len();
        println!(
            "seq {:>5} · digest {:>8} · triaged tail {:>8}{mark}",
            g.seq,
            kib(layout.digest_bytes),
            kib(layout.tail_bytes)
        );
    }
}

fn kib(bytes: usize) -> String {
    format!("{:.1} KiB", bytes as f64 / 1024.0)
}
