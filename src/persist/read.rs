//! Reading a persisted log back (docs/log-and-projection.md).
//!
//! Strict and read-only. Nothing here opens a log for writing, truncates one,
//! or repairs a torn tail: every failure is a [`LoadError`] naming the file and
//! the class of what went wrong, and a load never changes a byte. An inventory
//! pass and an offline viewer therefore see exactly what a writer finished, and
//! the repair of anything it did not is a separate, explicit, writable step
//! ([`super::open`]).
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

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::log::LogEntry;

use super::error::LoadError;
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
pub fn load_recording(path: impl AsRef<Path>) -> Result<LoadedRecording, LoadError> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|error| LoadError::io(path, error))?;
    load_recording_from(path, &file)
}

/// [`load_recording`] over a handle the caller already holds — a writable
/// reopen ([`super::open`]), which owns a lock on the file and so reads through
/// the same handle: where locks bind reads, a second one could not read the
/// bytes this writer is about to repair.
///
/// The read moves the handle's position, so a caller appending afterwards
/// positions it again rather than assuming where the load left it.
pub fn load_recording_from(
    path: impl AsRef<Path>,
    file: &File,
) -> Result<LoadedRecording, LoadError> {
    let path = path.as_ref();
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
            .map_err(|error| LoadError::io(path, error))?;
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
                return Err(LoadError::corrupt(
                    path,
                    line_number,
                    format!("malformed entry: not utf-8 ({error})"),
                ));
            }
        };
        if text.trim().is_empty() {
            return Err(LoadError::corrupt(
                path,
                line_number,
                "blank line: every line after the header is one entry",
            ));
        }
        match serde_json::from_str::<LogEntry>(text) {
            Ok(entry) => {
                let index = entries.len() as u64;
                if entry.seq != index {
                    return Err(LoadError::Sequence {
                        path: path.to_path_buf(),
                        line: line_number,
                        expected: index,
                        found: entry.seq,
                    });
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
                return Err(LoadError::corrupt(
                    path,
                    line_number,
                    format!("malformed entry: {error}"),
                ));
            }
        }
    }
    let Some(header) = header else {
        return Err(LoadError::corrupt(
            path,
            1,
            "the file is empty, so it has no header",
        ));
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
pub fn load(path: impl AsRef<Path>) -> Result<Vec<LogEntry>, LoadError> {
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

fn read_header(path: &Path, body: &[u8]) -> Result<LogHeader, LoadError> {
    let text = std::str::from_utf8(body).map_err(|error| {
        LoadError::corrupt(path, 1, format!("the header line is not utf-8: {error}"))
    })?;
    if text.trim().is_empty() {
        return Err(LoadError::corrupt(path, 1, "the header line is blank"));
    }
    serde_json::from_str::<LogHeader>(text).map_err(|error| classify_header(path, text, &error))
}

/// Name *which* way line 1 is not a header this build reads, so a caller can
/// tell a file it must leave alone from a log it has damaged. The refusal was
/// already decided by the header's own deserializer; this reads the line once
/// more only to say why it said no — the three classes a caller acts on
/// differently are named, and everything else keeps serde's wording of the
/// damage inside [`LoadError::Corrupt`].
fn classify_header(path: &Path, text: &str, error: &serde_json::Error) -> LoadError {
    let damaged = || LoadError::corrupt(path, 1, format!("unreadable header: {error}"));
    let Ok(value) = serde_json::from_str(text) else {
        return damaged();
    };
    let serde_json::Value::Object(fields) = value else {
        return LoadError::corrupt(path, 1, "the header line is not a JSON object");
    };
    match fields.get("format") {
        // No `format` is the unversioned log — a file written before the header
        // existed — or a file that is not a session log. Neither is read.
        None => LoadError::Unversioned {
            path: path.to_path_buf(),
        },
        Some(serde_json::Value::String(found)) if found != LogHeader::FORMAT => {
            LoadError::WrongFormat {
                path: path.to_path_buf(),
                found: found.clone(),
            }
        }
        // The right format: the version decides, and it is settled before the
        // metadata is read, so a newer file reports as the version it is.
        Some(serde_json::Value::String(_)) => {
            match fields.get("version").and_then(serde_json::Value::as_u64) {
                Some(version) if version != u64::from(LogHeader::VERSION) => {
                    LoadError::UnsupportedVersion {
                        path: path.to_path_buf(),
                        version,
                    }
                }
                // Absent, not a whole number, or the version this build reads with
                // metadata it cannot read — all damage, and serde has said which.
                _ => damaged(),
            }
        }
        Some(_) => damaged(),
    }
}
