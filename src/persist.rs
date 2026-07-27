//! Durable persistence of the log (docs/README.md).
//!
//! Append-only **JSONL**: one [`LogEntry`] per line. Chosen deliberately over a
//! binary format because the log is an append-only *event journal* we want to
//! read/`tail`/`jq` while iterating on projection — and because a Phase-1
//! redaction is itself just another appended event line (flip `wire`, never
//! touch `raw`), where a snapshot format would re-serialize the world. Perf is a
//! non-issue: projection reads the in-memory `Vec` each turn, never the disk;
//! the disk is touched once per append and once per session load.
//!
//! This is the durable persistence *mechanism*; `log.rs` holds the log
//! *types*. Phase 0 always constructs `wire: Raw`, so persisting the entry whole
//! is faithful; when Phase 1 introduces wire flips, those become their own
//! appended events that replay folds in (not a rewrite of the line).

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::log::{LogEntry, Payload, Seq, Timestamp, Turn};

/// The live append-only log: the in-memory entries plus an optional JSONL sink.
/// Construction is the only place that touches the clock and `seq` allocation,
/// so those non-deterministic inputs never leak toward the wire (docs/log-and-projection.md).
pub struct Log {
    entries: Vec<LogEntry>,
    sink: Option<BufWriter<File>>,
}

impl Log {
    /// A live log appending to `path` (created if absent; opened for append so a
    /// re-run never truncates a prior session).
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            entries: Vec::new(),
            sink: Some(BufWriter::new(file)),
        })
    }

    /// An in-memory-only log — ephemeral runs and tests; no disk.
    pub fn in_memory() -> Self {
        Self {
            entries: Vec::new(),
            sink: None,
        }
    }

    /// The entries so far — what [`crate::project::project`] folds into the wire.
    pub fn entries(&self) -> &[LogEntry] {
        &self.entries
    }

    /// Append a payload: assign the next `seq` (gap-free — append-only, so
    /// `seq == index`), stamp the wall clock, **durably write the JSONL line**,
    /// then keep it in memory. The line is flushed before returning so a crash
    /// can't lose an acknowledged entry (we stop short of `fsync` per line in
    /// Phase 0 — a deliberate durability/throughput tradeoff to revisit).
    pub fn append(&mut self, turn: Turn, payload: Payload) -> io::Result<Seq> {
        let seq = self.entries.len() as Seq;
        let entry = LogEntry {
            seq,
            turn,
            at: now(),
            payload,
        };
        if let Some(sink) = &mut self.sink {
            serde_json::to_writer(&mut *sink, &entry).map_err(io::Error::other)?;
            sink.write_all(b"\n")?;
            sink.flush()?;
        }
        self.entries.push(entry);
        Ok(seq)
    }
}

/// Replay a persisted log back into memory (the basis for daemon attach — the backlog).
/// Blank lines are tolerated; a malformed line is a hard error — a corrupt
/// journal should be loud, not silently truncated.
pub fn load(path: impl AsRef<Path>) -> io::Result<Vec<LogEntry>> {
    let file = File::open(path)?;
    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        entries.push(serde_json::from_str(&line).map_err(io::Error::other)?);
    }
    Ok(entries)
}

fn now() -> Timestamp {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis() as u64;
    Timestamp(millis)
}

#[cfg(test)]
mod test;
