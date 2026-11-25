//! Durable persistence of the log (docs/README.md).
//!
//! Append-only **JSONL**: one [`LogEntry`] per line. Chosen deliberately over a
//! binary format because the log is an append-only *event journal* we want to
//! read/`tail`/`jq` while iterating on projection — and because a
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
//! `seq` and `project` never receives it — and readers require it, so a headerless
//! log is refused rather than read on the assumption that it parses.
//! Creation writes that line before returning, so a file that exists is
//! already a readable log and a second run over the same path cannot append
//! to a `seq` space somebody else owns.
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
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

use std::sync::Arc;

use crate::record::log::{LogEntry, Payload, Seq, Timestamp, Turn};

mod error;
mod header;
mod open;
mod read;

pub use error::{LoadError, OpenError};
pub use header::{LogHeader, SessionMetadata};
pub use open::{Opened, Repair};
pub use read::{LoadedRecording, Trailing, load, load_recording, read_header};

/// Why a writer lock was refused. `Held` is another writer's lock and its
/// process is still appending; `Io` is the platform refusing for some other
/// reason. Kept apart from [`OpenError`]: `create`'s signature is `io::Result`,
/// so it maps the same refusal in its own terms.
enum LockRefused {
    Held,
    Io(io::Error),
}

/// Take the exclusive writer lock (docs/log-and-projection.md), so this
/// `File` is the log's only writer for as long as it is held — which is as long
/// as the [`Sink`] owning it lives. An existing lock is refused rather than
/// waited on: a second writer queued behind a live session would be a decision
/// made at a moment nothing is asking for, and the caller is told instead.
fn writer_lock(file: &File) -> Result<(), LockRefused> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(LockRefused::Held),
        Err(TryLockError::Error(error)) => Err(LockRefused::Io(error)),
    }
}

/// The writable end of a log, and the file's exclusive writer lock.
///
/// A buffered sink would leave a failed write's bytes split between its buffer
/// and the file, with the line's `seq` still free — so the next append would
/// reuse that `seq` behind bytes the reader rejects as a torn line. This writes
/// each line straight to the file instead, remembers the offset just past the
/// last complete line, and truncates back to it when a write fails: an append
/// that returns an error leaves the file exactly as it was.
///
/// A write that cannot be undone leaves the sink **poisoned**. The bytes after
/// `end` are then of unknown extent, so later appends are refused before writing
/// anything; only reopening the log repairs the tail (docs/log-and-projection.md).
struct Sink {
    /// The log file, holding the writer lock for as long as the sink lives.
    file: File,
    /// The offset just past the last complete line, which is where the next
    /// append begins and where a failed one is cut back to.
    end: u64,
    /// Set when a failed append could not be rolled back.
    failed: bool,
    /// One-shot fault injection for the partial-write test (process/testing.md):
    /// write this many bytes of the next line, then report an error.
    #[cfg(test)]
    inject_write_bytes: Option<usize>,
    /// One-shot fault injection: the rollback after the next failed write fails.
    #[cfg(test)]
    inject_rollback_failure: bool,
}

impl Sink {
    /// Wrap a file this `Log` holds the writer lock on, recording where its
    /// last complete line ends.
    fn at(mut file: File) -> io::Result<Self> {
        let end = file.stream_position()?;
        Ok(Self {
            file,
            end,
            failed: false,
            #[cfg(test)]
            inject_write_bytes: None,
            #[cfg(test)]
            inject_rollback_failure: false,
        })
    }

    /// Write one complete line, or leave the file as it was.
    fn append(&mut self, line: &[u8]) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other(
                "the log refuses appends: an earlier partial line could not be removed",
            ));
        }
        let prior = self.end;
        match self.write(line) {
            Ok(()) => {
                self.end = prior + line.len() as u64;
                Ok(())
            }
            Err(cause) => self.roll_back(prior, cause),
        }
    }

    fn write(&mut self, line: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if let Some(written) = self.inject_write_bytes.take() {
            let written = written.min(line.len());
            self.file.write_all(&line[..written])?;
            return Err(io::Error::other("injected partial write"));
        }
        self.file.write_all(line)
    }

    /// Cut the file back to `prior` after a failed write, and report the write's
    /// own error — the reason the append failed, not the cleanup that followed.
    ///
    /// If the truncation fails too, the sink is poisoned and the returned error
    /// names both failures, with the rollback as its source: it is the one that
    /// leaves bytes on disk no later append may write behind.
    fn roll_back(&mut self, prior: u64, cause: io::Error) -> io::Result<()> {
        match self.undo(prior) {
            Ok(()) => Err(cause),
            Err(rollback) => {
                self.failed = true;
                Err(io::Error::new(
                    rollback.kind(),
                    RollbackFailed {
                        write: cause,
                        rollback,
                    },
                ))
            }
        }
    }

    /// Truncate to `prior` and put the write position there.
    fn undo(&mut self, prior: u64) -> io::Result<()> {
        #[cfg(test)]
        if std::mem::take(&mut self.inject_rollback_failure) {
            return Err(io::Error::other("injected rollback failure"));
        }
        self.file.set_len(prior)?;
        self.file.seek(SeekFrom::Start(prior)).map(|_| ())
    }
}

/// An append that failed to write and whose partial line could not be removed.
/// Both failures are in the message; the rollback is the [`source`], because it
/// is what leaves the file unappendable.
///
/// [`source`]: std::error::Error::source
#[derive(Debug)]
struct RollbackFailed {
    write: io::Error,
    rollback: io::Error,
}

impl std::fmt::Display for RollbackFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "an append failed ({}) and its partial line could not be removed ({}); \
             the log refuses further appends",
            self.write, self.rollback
        )
    }
}

impl std::error::Error for RollbackFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.rollback)
    }
}

/// The live append-only log: the in-memory entries plus an optional JSONL sink.
/// Construction is the only place that touches the clock and `seq` allocation,
/// so those non-deterministic inputs never leak toward the wire (docs/log-and-projection.md).
pub struct Log {
    entries: Vec<LogEntry>,
    /// The JSONL sink, which also holds the file's exclusive writer lock: the
    /// lock lives on the `File` inside it, so dropping the `Log` (or [`Log::close`])
    /// releases it.
    sink: Option<Sink>,
    /// Set by [`Log::close`]: the session has ended, and an append is an error
    /// rather than an entry that reaches memory and no file.
    closed: bool,
    /// The `[env]` mask (docs/env.md). Every payload passes through it on the way
    /// in, which is what makes append the one choke point securing every
    /// downstream surface. None (the default) masks nothing.
    masker: Option<Arc<dyn PayloadMask>>,
}

/// What every payload passes through on its way into the log: the session's `[env]`
/// mask (docs/env.md), which `env_table`'s `Masker` implements.
pub trait PayloadMask: Send + Sync {
    /// Replace each masked value the payload's text carries with its marker, in place.
    fn mask_payload(&self, payload: &mut Payload);
}

impl Log {
    /// A live log writing to `path`, with `metadata` recorded as its header.
    ///
    /// Creation is exclusive: an existing file is an error and is left
    /// untouched, never truncated and never appended to. Opening an existing
    /// log for append while starting from an empty `entries` would restart
    /// `seq` at 0 inside a file that already numbers its entries, which every
    /// offline reader (`view`, `wire`, `triage`, the TUI fold) sees as one
    /// sequence with duplicates in it — so the refusal is the only way that
    /// stays unreachable, and it fails before anything is written.
    ///
    /// The header line is written before returning, so a file that exists at
    /// any point after this is a log a reader can open.
    ///
    /// The sink holds the exclusive writer lock for the `Log`'s lifetime. The
    /// directory it was given was minted exclusively too, so the lock is not
    /// what stops a second run here — it is what makes *every* sink hold one,
    /// including the one [`Log::open`] takes on a log that already exists.
    pub fn create(path: impl AsRef<Path>, metadata: Option<SessionMetadata>) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = path.as_ref();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(crate::record::session_dir::FILE_MODE)
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
        LogHeader::new(metadata).write_to(&mut file)?;
        let sink = Sink::at(file)?;
        Ok(Self {
            entries: Vec::new(),
            sink: Some(sink),
            closed: false,
            masker: None,
        })
    }

    /// An in-memory-only log — ephemeral runs and tests; no disk.
    pub fn in_memory() -> Self {
        Self {
            entries: Vec::new(),
            sink: None,
            closed: false,
            masker: None,
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
            masker: None,
        }
    }

    /// Install the session's `[env]` mask (docs/env.md). Set before the first
    /// append: masking is irreversible filtering *upstream* of the log, so an
    /// entry already written was written with whatever masker was in force.
    pub fn set_masker(&mut self, masker: Arc<dyn PayloadMask>) {
        self.masker = Some(masker);
    }

    /// Mask `payload` exactly as [`Self::append`] does, writing nothing (docs/env.md). A caller
    /// that has to know whether the log would rewrite a value — an admission refusing a grant
    /// whose path masking changes — masks a copy here instead of reimplementing the filter.
    pub fn mask_payload(&self, payload: &mut Payload) {
        if let Some(masker) = &self.masker {
            masker.mask_payload(payload);
        }
    }

    /// The entries so far — what [`crate::record::project::project`] folds into the wire.
    pub fn entries(&self) -> &[LogEntry] {
        &self.entries
    }

    /// Append a payload: assign the next `seq` (gap-free — append-only, so
    /// `seq == index`), stamp the wall clock, **write the JSONL line**, then
    /// keep it in memory. The line is written before returning, so a crash
    /// can't lose an acknowledged entry; one that fails to write is rolled back
    /// and keeps neither its bytes nor its `seq`. It stops short of `fsync` per
    /// line — a deliberate durability/throughput tradeoff.
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
        self.mask_payload(&mut payload);
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
            // Serialize the whole line first: the sink writes bytes, and only a
            // clean line is written to the file (docs/log-and-projection.md).
            let mut line = serde_json::to_vec(&entry).map_err(io::Error::other)?;
            line.push(b'\n');
            sink.append(&line)?;
        }
        self.entries.push(entry);
        Ok(seq)
    }

    /// Make the next append fail after writing `written` bytes of its line, and
    /// optionally make the rollback that follows fail too: the two states the
    /// partial-write tests need, set here rather than reached through a race
    /// (process/testing.md).
    #[cfg(test)]
    fn fail_next_append(&mut self, written: usize, rollback_fails: bool) {
        let sink = self
            .sink
            .as_mut()
            .expect("fault injection needs a log that writes to disk");
        sink.inject_write_bytes = Some(written);
        sink.inject_rollback_failure = rollback_fails;
    }

    /// End the log's writing: release the file's writer lock now, rather than
    /// when the last holder of the `Log` lets go of it. A session's teardown
    /// calls this, so a log can be reopened as soon as its session has ended
    /// even while a reader (an SSE stream, a handler) still holds the session.
    /// The entries stay readable; appends after it fail.
    pub fn close(&mut self) {
        self.closed = true;
        // Nothing is buffered: each append wrote its line through. Dropping the
        // sink closes the file and releases its lock. A poisoned sink's tail is
        // left for `open`'s repair, exactly as a killed writer's would be.
        drop(self.sink.take());
    }

    /// Whether [`close`](Self::close) has run.
    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

#[cfg(test)]
mod test;
