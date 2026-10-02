//! Reopening a log for append (docs/log-and-projection.md).
//!
//! Reading a log and continuing one are separate operations, and only the
//! second one changes a file. [`Log::open`] is the whole of the second, in an
//! order that makes each step the thing that could still go wrong:
//!
//! 1. open the file for reading and writing, **without** creating it — a path
//!    with no log at it is a refusal, since `create` is what mints a session
//!    and this continues one;
//! 2. take the exclusive writer lock, which the writer that is already appending
//!    holds — [`super::writer_lock`];
//! 3. read the log with the same strict reader every other reader uses, and
//!    refuse anything it refuses, having written nothing;
//! 4. resolve whatever the load reported past the last entry — a [`Repair`];
//! 5. position at the end, so the next append is the next `seq`.
//!
//! The lock comes before the read on purpose. The entries loaded here are the
//! entries this writer continues, and a repair is computed against the bytes it
//! just read; a writer that read first and locked after could compute that
//! repair from a file another writer was already appending to, and write it
//! into the wrong place.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::error::OpenError;
use super::header::LogHeader;
use super::read::{Trailing, load_recording_from};
use super::{LockRefused, Log, writer_lock};

/// What an open had to resolve past the last entry before it could append. The
/// variants are the [`Trailing`] cases of the same name: this is the answer to
/// what a load reported, not a different account of the damage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Repair {
    /// The last entry was complete and lost only the newline that would have
    /// closed it, which was written. Every entry it read is kept.
    MissingNewline {
        /// Where that entry's bytes start in the file.
        byte_offset: u64,
    },
    /// A final append that never became an entry: its bytes are kept beside the
    /// log and the log is truncated to where they began, so the entries behind
    /// them stand and the next append begins a fresh line. The `seq` the
    /// fragment was reaching for is free again.
    PartialEntry {
        /// Where the dropped bytes started — the offset an entry would have
        /// begun at.
        byte_offset: u64,
        /// The file they were kept in, beside the log.
        saved_to: PathBuf,
        /// How many bytes were kept.
        bytes: usize,
    },
}

/// A log reopened for append.
pub struct Opened {
    /// The live log: the entries it read, and a sink that appends from where
    /// they end.
    pub log: Log,
    /// The header as it was read from the file, which a caller describes the
    /// session from. [`Log::open`] does not rewrite it.
    pub header: LogHeader,
    /// What the tail needed, or `None` on a log nobody was interrupted on.
    pub repaired: Option<Repair>,
}

impl Log {
    /// Reopen `path` to append to it, as [`Log::create`] would have made it.
    ///
    /// Returns the header as it was read and what had to be repaired to append
    /// at all, so a caller can say a session was continued from a torn tail.
    /// The session's `[env]` mask is installed afterwards with
    /// [`Log::set_masker`], as boot installs it for a log it created: an entry
    /// appended before then is written with whatever mask was in force, which
    /// is none.
    ///
    /// A file that is not a log this build reads is refused with the reason
    /// ([`OpenError::Load`]), and a log another writer holds with
    /// [`OpenError::Held`] — in both cases without writing a byte to it.
    pub fn open(path: impl AsRef<Path>) -> Result<Opened, OpenError> {
        let path = path.as_ref();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| OpenError::io(path, error))?;
        writer_lock(&file).map_err(|refused| match refused {
            LockRefused::Held => OpenError::Held {
                path: path.to_path_buf(),
            },
            LockRefused::Io(error) => OpenError::io(path, error),
        })?;
        // Through the handle that holds the lock, rather than a second open of
        // the path: where locks bind reads, a second handle could not read the
        // bytes this writer is about to repair.
        let recording = load_recording_from(path, &file).map_err(OpenError::Load)?;
        let repaired = repair_tail(path, &mut file, recording.trailing.as_ref())?;
        // The load read to the end of the file, and a repair moved the position
        // wherever it finished — a truncation leaves it past the new end — so
        // the sink's position is set rather than assumed.
        file.seek(SeekFrom::End(0))
            .map_err(|error| OpenError::io(path, error))?;
        Ok(Opened {
            log: Log {
                entries: recording.entries,
                sink: Some(BufWriter::new(file)),
                masker: Arc::default(),
            },
            header: recording.header,
            repaired,
        })
    }
}

/// Resolve whatever a load reported past the last complete entry, so the appends
/// after it continue a file whose lines are all whole, and say what was done.
///
/// A complete entry that lost its newline gets that one byte. A fragment that
/// never became an entry is kept beside the log — those bytes are the only copy
/// of whatever the interrupted append was carrying — and the log is truncated to
/// where they began. Nothing that read the log ever took the fragment for an
/// entry, so what it cost is the append it did not finish.
fn repair_tail(
    path: &Path,
    file: &mut File,
    trailing: Option<&Trailing>,
) -> Result<Option<Repair>, OpenError> {
    let Some(trailing) = trailing else {
        return Ok(None);
    };
    let repair = match trailing {
        Trailing::MissingNewline { byte_offset } => {
            file.seek(SeekFrom::End(0))
                .map_err(|error| OpenError::io(path, error))?;
            file.write_all(b"\n")
                .map_err(|error| OpenError::io(path, error))?;
            Repair::MissingNewline {
                byte_offset: *byte_offset,
            }
        }
        Trailing::PartialEntry { byte_offset, bytes } => {
            // The fragment is kept before the log is cut, so an open interrupted
            // between the two leaves a log that still has it.
            let saved_to = fragment_path(path, *byte_offset);
            keep_fragment(&saved_to, bytes).map_err(|error| OpenError::io(&saved_to, error))?;
            file.set_len(*byte_offset)
                .map_err(|error| OpenError::io(path, error))?;
            Repair::PartialEntry {
                byte_offset: *byte_offset,
                saved_to,
                bytes: bytes.len(),
            }
        }
    };
    file.sync_all()
        .map_err(|error| OpenError::io(path, error))?;
    Ok(Some(repair))
}

/// Where a dropped fragment is kept: beside the log, named for the offset it
/// began at, so two fragments of one file never contend for one name.
fn fragment_path(log: &Path, byte_offset: u64) -> PathBuf {
    let mut name = log.as_os_str().to_os_string();
    name.push(format!(".torn-{byte_offset}"));
    PathBuf::from(name)
}

/// Write the fragment that is about to be dropped out of the log.
///
/// A file already at that name is not overwritten: its bytes are an earlier
/// fragment, and the ones in hand are about to leave the only file that held
/// them. The one case that is not a collision is a repair interrupted *after*
/// this write — the same offset and the same bytes — which is finished here
/// rather than refused, so an open that died between the two steps does not
/// leave a log that cannot be reopened.
fn keep_fragment(saved_to: &Path, bytes: &[u8]) -> io::Result<()> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(saved_to)
    {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.sync_all()
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if std::fs::read(saved_to).is_ok_and(|kept| kept == bytes) {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a fragment from an earlier repair is already here, and these are not its bytes",
                ))
            }
        }
        Err(error) => Err(error),
    }
}
