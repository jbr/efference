//! Reading a persisted log back (docs/log-and-projection.md).
//!
//! Strict and read-only. Nothing here opens a log for writing, truncates one,
//! or repairs a torn tail: every failure is an error carrying the path and the
//! line it was found on, and a load never changes a byte. An inventory pass
//! and an offline viewer therefore see exactly what a writer finished, and the
//! repair of anything it did not is a separate, explicit, writable step.
//!
//! What is checked, in the order a reader needs it:
//!
//! - line 1 is a header of the format and version this build reads
//!   ([`LogHeader`]); an unversioned log is refused, not imported;
//! - every line after it is one entry — a blank line or an unparseable one is
//!   a hard error, because a log is a machine-written journal and a hole in
//!   one means something is wrong that a reader cannot rule out;
//! - `seq` equals the entry's index, so a duplicated or skipped `seq` cannot
//!   read as one continuous sequence to a consumer keying on it;
//! - bytes after the last complete entry are *reported*
//!   ([`Trailing`]), never repaired and never mistaken for an entry.
//!
//! Lines are read as bytes and decoded per line, because a write interrupted
//! mid-append can leave the tail cut through a multi-byte codepoint. Those
//! bytes are reported rather than turned into a decode error, which is the
//! one place this reader distinguishes a torn write from damage: only a final
//! unterminated line whose parse *ended prematurely* is a torn write. A
//! complete object that is not an entry — an unknown payload shape, a missing
//! field — is damage, and fails whether or not a newline closes it.

use std::fmt::Display;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use crate::log::LogEntry;

use super::header::LogHeader;

/// What one load found past the last complete entry — the bytes a writer left
/// behind when it was interrupted mid-append. Read-only loading reports this
/// and leaves the file alone; deciding what to do with it belongs to a
/// writable path that has to reason about who else may be writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trailing {
    /// The last entry is complete and its `seq` is in order; only its
    /// terminating newline is missing, which is what a crash between writing
    /// the object and writing the newline leaves. Every entry is usable — a
    /// writer appending here only has to terminate the line already there.
    MissingNewline {
        /// Where that entry's bytes start in the file.
        byte_offset: u64,
    },
    /// An entry the writer never finished. It is the last line in the file and
    /// the parse ran out of input inside it, so it is the only reading
    /// available: bytes to drop, or a line to complete.
    PartialEntry {
        /// Where those bytes start in the file — the offset an entry would
        /// have begun at.
        byte_offset: u64,
        /// The bytes as they stand: unterminated, and possibly not valid UTF-8
        /// if the tear landed inside a codepoint.
        bytes: Vec<u8>,
    },
}

/// One log as a reader found it: its header, the entries it could stand behind,
/// and whatever followed them.
#[derive(Clone, Debug)]
pub struct LoadedRecording {
    pub header: LogHeader,
    pub entries: Vec<LogEntry>,
    /// `None` when the file ends on the newline of its last entry — a log no
    /// writer was interrupted on. `Some` when it does not.
    pub trailing: Option<Trailing>,
}

impl LoadedRecording {
    /// Whether the log ends in bytes an append would have to resolve first.
    pub fn is_torn(&self) -> bool {
        self.trailing.is_some()
    }
}

/// Read `path` without touching it, keeping the header alongside the entries.
///
/// See [`Trailing`] for what happens to an interrupted final append: it is
/// reported, and this function writes nothing either way.
pub fn load_recording(path: impl AsRef<Path>) -> io::Result<LoadedRecording> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|error| at(path, error))?;
    let mut reader = BufReader::new(file);
    let mut header = None;
    let mut entries: Vec<LogEntry> = Vec::new();
    let mut trailing = None;
    let mut offset = 0u64;
    let mut line_number = 0usize;
    let mut raw = Vec::new();
    loop {
        raw.clear();
        let read = reader
            .read_until(b'\n', &mut raw)
            .map_err(|error| at(path, error))?;
        if read == 0 {
            break;
        }
        let start = offset;
        offset += read as u64;
        line_number += 1;
        let terminated = raw.last() == Some(&b'\n');
        let body = line_body(&raw);

        if line_number == 1 {
            header = Some(read_header(path, body)?);
            continue;
        }
        let text = match std::str::from_utf8(body) {
            Ok(text) => text,
            // Unterminated, so the last line in the file, and not text at all:
            // a tear inside a codepoint. Report the bytes rather than fail the
            // entries in front of them.
            Err(_) if !terminated => {
                trailing = Some(partial(start, body));
                continue;
            }
            Err(error) => {
                return Err(invalid(
                    path,
                    line_number,
                    format!("malformed entry: not utf-8 ({error})"),
                ));
            }
        };
        if text.trim().is_empty() {
            return Err(invalid(
                path,
                line_number,
                "blank line: every line after the header is one entry",
            ));
        }
        match serde_json::from_str::<LogEntry>(text) {
            Ok(entry) => {
                let index = entries.len() as u64;
                if entry.seq != index {
                    return Err(invalid(
                        path,
                        line_number,
                        format!(
                            "entry carries seq {} but sits at index {index}: seq must equal the \
                             entry's index, so a duplicated or skipped seq is a corrupt log",
                            entry.seq
                        ),
                    ));
                }
                entries.push(entry);
                if !terminated {
                    trailing = Some(Trailing::MissingNewline { byte_offset: start });
                }
            }
            // Only a parse that ran out of input is a torn append; see the
            // module docs for why a complete object that is not an entry is not.
            Err(error) if !terminated && error.is_eof() => {
                trailing = Some(partial(start, body));
            }
            Err(error) => {
                return Err(invalid(
                    path,
                    line_number,
                    format!("malformed entry: {error}"),
                ));
            }
        }
    }
    let Some(header) = header else {
        return Err(invalid(path, 1, "the file is empty, so it has no header"));
    };
    Ok(LoadedRecording {
        header,
        entries,
        trailing,
    })
}

/// The entries of the log at `path` — [`load_recording`] for a reader that
/// only wants the record. A trailing partial append is dropped along with the
/// [`Trailing`] that describes it, so an offline viewer shows a session's
/// intact turns; a caller that must not lose those bytes asks for the whole
/// [`LoadedRecording`].
pub fn load(path: impl AsRef<Path>) -> io::Result<Vec<LogEntry>> {
    load_recording(path).map(|recording| recording.entries)
}

/// One line without its `\n`/`\r\n`. Whether the newline was there is the
/// caller's business, since it is what separates a complete line from a torn
/// one.
fn line_body(raw: &[u8]) -> &[u8] {
    let body = raw.strip_suffix(b"\n").unwrap_or(raw);
    body.strip_suffix(b"\r").unwrap_or(body)
}

fn partial(byte_offset: u64, bytes: &[u8]) -> Trailing {
    Trailing::PartialEntry {
        byte_offset,
        bytes: bytes.to_vec(),
    }
}

fn read_header(path: &Path, body: &[u8]) -> io::Result<LogHeader> {
    let text = std::str::from_utf8(body)
        .map_err(|error| invalid(path, 1, format!("the header line is not utf-8: {error}")))?;
    if text.trim().is_empty() {
        return Err(invalid(path, 1, "the header line is blank"));
    }
    serde_json::from_str::<LogHeader>(text)
        .map_err(|error| invalid(path, 1, format!("unreadable header: {error}")))
}

fn invalid(path: &Path, line: usize, message: impl Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}:{line}: {message}", path.display()),
    )
}

fn at(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}
