//! The background-process panel's rows (docs/tui.md, docs/process-monitor.md): the
//! `processes` topic's listed processes, each with the output tail this client collects
//! and the elapsed times it derives.
//!
//! The topic carries structure only: output arrives as `process_output` deltas and
//! changes per chunk, so a level-triggered topic that carried it would be re-sent at the
//! rate of the output (docs/control-api.md). The tail is therefore this client's own, and
//! a topic replacement must not wipe it: [`panel`] carries each tail across by process id.

use super::secs_since;
use crate::protocol::state::ProcessState;

/// Cap on a row's tail. The panel shows the last line only, and the next topic
/// replacement is the authoritative list of rows, so this bounds the gap-filling append.
pub(crate) const TAIL_CAP: usize = 4096;

/// Whether the panel lists a process: one that runs in the background and has not been
/// dismissed from view. The topic carries every running process, flagged rather than
/// filtered (docs/control-api.md); the foreground tool already renders live in the
/// transcript, and a muted process still notifies on its exit.
pub(crate) fn listed(process: &ProcessState) -> bool {
    process.background && !process.muted
}

/// One live background process as the panel holds it: the topic's row, the tail
/// collected from its `process_output` deltas, and the display times.
///
/// `running_secs` and `idle_secs` are derived for display when the row changes, from the
/// absolute stamps beside them, so rendering stays a pure function of state (the
/// snapshot harness depends on that). The stamps are kept so the clock tick can derive
/// them again without a round trip (`refresh_clocks`).
#[derive(Debug, Clone)]
pub(crate) struct BgProc {
    pub process: ProcessState,
    /// Absolute Unix millis, stamped locally on each `process_output` delta. `None` until
    /// the first chunk, which the topic cannot supply without re-sending per chunk.
    pub last_chunk_at: Option<u64>,
    /// Accumulated from `process_output` deltas, seeded on first sight from
    /// `GET /processes/:id/output` (the bridge's cold-open fetch, docs/control-api.md).
    pub tail: String,
    pub running_secs: u64,
    pub idle_secs: u64,
}

impl BgProc {
    /// A row for `process`, with no output yet.
    pub(crate) fn new(process: ProcessState) -> Self {
        let mut row = Self {
            process,
            last_chunk_at: None,
            tail: String::new(),
            running_secs: 0,
            idle_secs: 0,
        };
        row.refresh_elapsed();
        row
    }

    /// Derive the display times from the stamps. Called when the row changes, and once a
    /// second by the clock tick (`refresh_clocks`).
    pub(crate) fn refresh_elapsed(&mut self) {
        self.running_secs = secs_since(self.process.spawned_at.0);
        self.idle_secs = self.last_chunk_at.map_or(0, secs_since);
    }

    /// The prose form of the id (`p#3`) — what the panel shows, matching the vocabulary
    /// the model and the transcript use.
    pub(crate) fn handle(&self) -> String {
        crate::process_monitor::handle(self.process.id)
    }

    /// Append a `process_output` chunk to the tail and stamp the arrival. The tail is
    /// bounded: it is cut from the front on a char boundary, since only the last line is
    /// shown.
    pub(crate) fn push_output(&mut self, chunk: &str) {
        self.last_chunk_at = Some(crate::log::Timestamp::now().0);
        self.refresh_elapsed();
        self.tail.push_str(chunk);
        if self.tail.len() > TAIL_CAP {
            let from = self.tail.len() - TAIL_CAP;
            let from = (from..=self.tail.len())
                .find(|&i| self.tail.is_char_boundary(i))
                .unwrap_or(self.tail.len());
            self.tail.drain(..from);
        }
    }
}

/// The panel for a `processes` topic value: its [`listed`] processes, each carrying the
/// tail and stamp of the row `previous` held for the same id. A process that has since
/// exited leaves with its tail.
pub(crate) fn panel(processes: &[ProcessState], previous: &[BgProc]) -> Vec<BgProc> {
    processes
        .iter()
        .filter(|process| listed(process))
        .map(|process| {
            let mut row = BgProc::new(process.clone());
            if let Some(held) = previous.iter().find(|p| p.process.id == process.id) {
                row.tail.clone_from(&held.tail);
                row.last_chunk_at = held.last_chunk_at;
                row.refresh_elapsed();
            }
            row
        })
        .collect()
}
