//! What one process was, live or exited — the representation `GET /processes/:id`
//! answers with (docs/process-monitor.md, docs/control-api.md).
//!
//! Two sources produce it, in this order:
//!
//! - the daemon's in-memory registry, which is richest while the daemon that
//!   spawned the process is running (it knows `muted`, and it holds the capture
//!   the sibling output route reads);
//! - a fold over the log's [`ProcessEvent`] entries, which is what a recorded
//!   session or a restarted daemon has. Output may be unavailable there; the
//!   metadata still answers.
//!
//! Both spellings carry the same field names as [`ProcessState`], the listing's
//! representation, so a client parsing either sees one vocabulary.
//!
//! [`ProcessState`]: super::ProcessState

use serde::Serialize;

#[cfg(test)]
mod test;

use crate::log::{LogEntry, Payload, ProcessEvent, ProcessEventKind, ProcessExit, Timestamp};

/// One process, running or exited — the durable answer to "what was `p#<n>`"
/// (docs/process-monitor.md).
#[derive(Clone, Debug, Serialize)]
pub struct ProcessRecord {
    /// The spawn-minted ordinal (rendered `p#<n>` in prose; bare here and in routes).
    pub id: u64,
    pub command: String,
    /// The `tool_use` id of the call that spawned this process.
    pub call_id: Option<String>,
    /// The OS pid — an attribute, never the identifier (docs/process-monitor.md). For
    /// a log-derived record it is a breadcrumb for reading contemporaneous system
    /// logs: pids are recycled, so it addresses nothing after the fact.
    pub pid: Option<u32>,
    /// Absolute Unix millis, as [`ProcessState`](super::ProcessState) carries it.
    pub spawned_at: Timestamp,
    pub background: bool,
    /// Dismissed-from-view. A property of a *live* view, so a log-derived record
    /// reads `false` — the log records what a process was, not what a since-dead
    /// daemon was showing.
    pub muted: bool,
    /// `false` once the process has exited. The field the listing's
    /// representation has no need for (it carries only running processes) and
    /// this one does, because it answers for both.
    pub running: bool,
    /// How it ended: status, duration, and when. `None` while it runs — and also
    /// for a process whose exit was never recorded, which is what a daemon killed
    /// mid-run leaves behind.
    pub exit: Option<ProcessExit>,
}

/// Fold the log for one process's record (docs/process-monitor.md): the mint entry,
/// plus the exit entry if one was written. `None` if no [`ProcessEvent`] names
/// this id — a handle that was never backgrounded, or a log written before the
/// record existed. Order-defined and last-write-wins per half, like every other
/// fold over the log.
pub fn from_log(entries: &[LogEntry], id: u64) -> Option<ProcessRecord> {
    let mut record: Option<ProcessRecord> = None;
    let mut exit: Option<ProcessExit> = None;
    for entry in entries {
        let Payload::ProcessEvent(ProcessEvent { id: at_id, kind }) = &entry.payload else {
            continue;
        };
        if *at_id != id {
            continue;
        }
        match kind {
            ProcessEventKind::Spawned(spawn) => {
                record = Some(ProcessRecord {
                    id,
                    command: spawn.command.clone(),
                    call_id: spawn.call_id.as_ref().map(|c| c.0.clone()),
                    pid: spawn.pid,
                    spawned_at: spawn.at,
                    // Every recorded mint is a mint into the background namespace
                    // (docs/process-monitor.md): a foreground command's record is its
                    // own call and result.
                    background: true,
                    muted: false,
                    // A record only reaches the fold when the live registry
                    // didn't answer, so this process is not running *here*
                    // whatever the log says. With no exit entry the end was
                    // simply never observed — the daemon died first — and `exit:
                    // None` beside `running: false` says exactly that.
                    running: false,
                    exit: None,
                });
            }
            ProcessEventKind::Exited(recorded) => exit = Some(*recorded),
        }
    }
    let mut record = record?;
    record.exit = exit;
    Some(record)
}
