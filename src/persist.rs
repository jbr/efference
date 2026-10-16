//! Durable persistence of the log (docs/README.md).
//!
//! Append-only **JSONL**: one [`LogEntry`] per line. Chosen deliberately over a
//! binary format because the log is an append-only *event journal* we want to
//! read/`tail`/`jq` while iterating on projection — and because a Phase-1
//! triage is itself just another appended event line (flip `wire`, never
//! touch `raw`), where a snapshot format would re-serialize the world. Perf is a
//! non-issue: projection reads the in-memory `Vec` each turn, never the disk;
//! the disk is touched once per append and once per session load.
//!
//! This is the durable persistence *mechanism*; `log.rs` holds the log
//! *types*. An entry is persisted whole at append and its line is never
//! rewritten: a later curation decision is its own appended event that replay
//! folds in (docs/log-and-projection.md).
//!
//! A file is a **versioned** log: line 1 is a header carrying the format and
//! version plus the session metadata boot recorded ([`LogHeader`]), and the
//! entries follow from index 0. The header is not an entry — it takes no
//! `seq` and never reaches `project` — and readers require it, so a log
//! written before the header is refused rather than read on the assumption
//! that it parses. [`Log::create`] creates its file exclusively and writes
//! that line before returning, so a file that exists is already a readable
//! log and a second run over the same path cannot append to a `seq` space
//! somebody else owns.
//!
//! A log has one writer. Every sink holds an **exclusive lock** on its file for
//! as long as it is open — taken by [`Log::create`] and by [`Log::open`],
//! released by [`Log::close`] or when the `Log` drops — and an existing lock is refused rather than
//! waited on, so two writers (in one host or in two) cannot interleave their
//! appends into one `seq` space. Reading a log takes no lock, so a session
//! running somewhere else can still be read.
//!
//! [`header`] holds the header and metadata types, [`read`] the strict
//! read-only loader, [`open`] the writable reopen, [`error`] what either can
//! refuse.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufWriter, Write};
use std::path::Path;

use std::sync::Arc;

use crate::env_table::Masker;
use crate::log::{LogEntry, Payload, Seq, Timestamp, Turn};

mod error;
mod header;
mod open;
mod read;

pub use error::{LoadError, OpenError};
pub use header::{LogHeader, SessionMetadata, TriageModel};
pub use open::{Opened, Repair};
pub use read::{LoadedRecording, Trailing, load, load_recording, read_header};

/// Why a writer lock was refused. `Held` is another writer's lock and its
/// process is still appending; `Io` is the platform refusing for some other
/// reason. Kept apart from [`OpenError`] so `create`, whose signature is
/// `io::Result` and predates a typed open path, maps the same refusal in its
/// own terms.
enum LockRefused {
    Held,
    Io(io::Error),
}

/// Take the exclusive writer lock (docs/log-and-projection.md), so this
/// `File` is the log's only writer for as long as it is held — which is as long
/// as the `BufWriter` owning it lives. An existing lock is refused rather than
/// waited on: a second writer queued behind a live session would be a decision
/// made at a moment nothing is asking for, and the caller is told instead.
fn writer_lock(file: &File) -> Result<(), LockRefused> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(LockRefused::Held),
        Err(TryLockError::Error(error)) => Err(LockRefused::Io(error)),
    }
}

/// The live append-only log: the in-memory entries plus an optional JSONL sink.
/// Construction is the only place that touches the clock and `seq` allocation,
/// so those non-deterministic inputs never leak toward the wire (docs/log-and-projection.md).
pub struct Log {
    entries: Vec<LogEntry>,
    /// The JSONL sink, which also holds the file's exclusive writer lock: the
    /// lock lives on the `File` inside it, so dropping the `Log` (or [`Log::close`])
    /// flushes the last line and then releases it.
    sink: Option<BufWriter<File>>,
    /// Set by [`Log::close`]: the session has ended, and an append is an error
    /// rather than an entry that reaches memory and no file.
    closed: bool,
    /// The `[env]` mask (docs/env.md). Every payload passes through it on the way
    /// in, which is what makes append the one choke point securing every
    /// downstream surface — the wire, the jsonl, the TUI, `control transcript` and `entries`. An empty
    /// masker (the default, and every test that does not install one) is the
    /// identity.
    masker: Arc<Masker>,
}

impl Log {
    /// A live log writing to `path`, with `metadata` recorded as its header.
    ///
    /// Creation is exclusive: an existing file is an error and is left
    /// untouched, never truncated and never appended to. Opening an existing
    /// log for append while starting from an empty `entries` would restart
    /// `seq` at 0 inside a file that already numbers its entries, which every
    /// offline reader (`view`, `wire`, `peek`, the TUI fold) sees as one
    /// sequence with duplicates in it — so the refusal is the only way that
    /// stays unreachable, and it fails before anything is written.
    ///
    /// The header line is written and flushed before returning, so a file that
    /// exists at any point after this is a log a reader can open.
    ///
    /// The sink holds the exclusive writer lock for the `Log`'s lifetime. The
    /// directory it was given was minted exclusively too, so the lock is not
    /// what stops a second run here — it is what makes *every* sink hold one,
    /// including the one [`Log::open`] takes on a log that already exists.
    pub fn create(path: impl AsRef<Path>, metadata: Option<SessionMetadata>) -> io::Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                io::Error::new(error.kind(), format!("{}: {error}", path.display()))
            })?;
        match writer_lock(&file) {
            Ok(()) => {}
            Err(LockRefused::Held) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("{}: another writer holds this log open", path.display()),
                ));
            }
            Err(LockRefused::Io(error)) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("{}: {error}", path.display()),
                ));
            }
        }
        let mut sink = BufWriter::new(file);
        LogHeader::new(metadata).write_to(&mut sink)?;
        sink.flush()?;
        Ok(Self {
            entries: Vec::new(),
            sink: Some(sink),
            closed: false,
            masker: Arc::default(),
        })
    }

    /// An in-memory-only log — ephemeral runs and tests; no disk.
    pub fn in_memory() -> Self {
        Self {
            entries: Vec::new(),
            sink: None,
            closed: false,
            masker: Arc::default(),
        }
    }

    /// An in-memory log seeded with entries replayed from disk ([`load`]) — the
    /// shape a daemon reading a session it did not itself run has. No sink: this
    /// stands in front of a record someone else wrote, so it must not append to
    /// it. `seq` stays index-equal, because appends continue from the seeded
    /// length.
    pub fn in_memory_with(entries: Vec<LogEntry>) -> Self {
        Self {
            entries,
            sink: None,
            closed: false,
            masker: Arc::default(),
        }
    }

    /// Install the session's `[env]` mask (docs/env.md). Set before the first
    /// append: masking is irreversible filtering *upstream* of the log, so an
    /// entry already written was written with whatever masker was in force.
    pub fn set_masker(&mut self, masker: Arc<Masker>) {
        self.masker = masker;
    }

    /// The entries so far — what [`crate::project::project`] folds into the wire.
    pub fn entries(&self) -> &[LogEntry] {
        &self.entries
    }

    /// Append a payload: assign the next `seq` (gap-free — append-only, so
    /// `seq == index`), stamp the wall clock, **durably write the JSONL line**,
    /// then keep it in memory. The line is flushed before returning so a crash
    /// can't lose an acknowledged entry. It stops short of `fsync` per line — a
    /// deliberate durability/throughput tradeoff.
    pub fn append(&mut self, turn: Turn, payload: Payload) -> io::Result<Seq> {
        self.append_submission(turn, payload, None, None)
    }

    /// [`append`](Self::append) with the submission provenance of the
    /// `POST /v1/turns` the entry realizes (docs/control-api.md) — the loop's
    /// user-turn appends use this; everything else appends without one. `source`
    /// is the client's self-chosen label and is projected on a `User` entry's
    /// wire block when present (docs/log-and-projection.md).
    pub fn append_submission(
        &mut self,
        turn: Turn,
        payload: Payload,
        submission_id: Option<String>,
        source: Option<String>,
    ) -> io::Result<Seq> {
        // The mask choke point (docs/env.md). Both `append` paths funnel here, so
        // every text-bearing payload — tool results, the boot preamble, ambient
        // notifications, user paste, harness error text — is filtered exactly
        // once, before it becomes a durable record. This is deliberately *not* a
        // `WireState`: there is no reversible verdict, because a raw copy on disk
        // is half of what masking exists to prevent (docs/log-and-projection.md).
        if self.closed {
            return Err(io::Error::other("the log is closed: its session has ended"));
        }
        let mut payload = payload;
        self.masker.mask_payload(&mut payload);
        let seq = self.entries.len() as Seq;
        let entry = LogEntry {
            seq,
            turn,
            at: Timestamp::now(),
            submission_id,
            source,
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

    /// End the log's writing: flush the sink and release the file's writer lock
    /// now, rather than when the last holder of the `Log` lets go of it. A
    /// session's teardown calls this, so a log can be reopened as soon as its
    /// session has ended even while a reader (an SSE stream, a handler) still
    /// holds the session. The entries stay readable; appends after it fail.
    pub fn close(&mut self) -> io::Result<()> {
        self.closed = true;
        match self.sink.take() {
            Some(mut sink) => sink.flush(),
            None => Ok(()),
        }
    }

    /// Whether [`close`](Self::close) has run.
    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

#[cfg(test)]
mod test;
