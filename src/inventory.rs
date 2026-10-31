//! The archived-session inventory: a filesystem scan of one working root's
//! `.efference/` directory (docs/log-and-projection.md).
//!
//! A session is a directory holding a `log.jsonl`, and the log answers every
//! question a listing asks. The scan needs no host: it reads each log's header
//! line, its first entry and its last complete entry — the last by seeking from
//! the end — and the title by reading backwards in chunks to the last
//! `Payload::Title`. A log is never parsed whole, so cost per log does not grow
//! with its length except for the title of a log that has none.
//!
//! Recency is the `at` of entries, never file modification time: a migration
//! that rewrites a file moves its mtime without any session having been active.
//!
//! A directory that cannot be listed as a session is a row with a `problem`
//! rather than an omission — a missing log, an empty one, a header this build
//! does not read, a first line that is not a boot preamble (`boot::resume`
//! refuses such a log), a first or last line that does not parse. The one line that is
//! not a problem is a torn final line without a newline (an interrupted append,
//! [`crate::persist::Trailing`]): it is skipped back over.
//!
//! [`Inventory`] keeps the scanned rows in memory, keyed by log path with the
//! length the scan saw. Logs are append-only, so a changed length is a changed
//! log; a refresh stats each log and rescans only those.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

use crate::log::{LogEntry, Payload};
use crate::persist::read_header;

/// How much of the file one backwards read takes.
const CHUNK: u64 = 64 * 1024;

/// One session directory under a root's `.efference/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct InventoryRow {
    /// The directory name.
    pub id: String,
    /// The working root whose `.efference` holds the directory.
    pub cwd: PathBuf,
    /// The text of the last `Title` entry, when the log has one
    /// (docs/session-title.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `at` of the first entry after the header, epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    /// `at` of the last complete entry, epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<u64>,
    /// Set when the last lifecycle entry (`Resumed` or `Archived`) is not `Archived`:
    /// the session was open when its host went away, rather than archived on purpose.
    /// A log with neither entry is a session that was never resumed or archived, so
    /// it counts as open too (docs/resumption.md).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    /// Who archived the session, from the `source` of the `Archived` entry that is the
    /// last lifecycle entry; absent when it records none or the last lifecycle entry is
    /// not `Archived` (docs/log-and-projection.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_by: Option<String>,
    /// Why this directory is not a readable session, when it is not. A row with
    /// a problem keeps whatever the scan did learn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

impl InventoryRow {
    fn new(id: &str, cwd: &Path) -> Self {
        Self {
            id: id.to_string(),
            cwd: cwd.to_path_buf(),
            title: None,
            created_at: None,
            last_active_at: None,
            interrupted: false,
            archived_by: None,
            problem: None,
        }
    }

    fn with_problem(mut self, problem: impl Into<String>) -> Self {
        self.problem = Some(problem.into());
        self
    }
}

/// Most recently active first; rows with no timestamp last, then by id.
pub fn sort_rows(rows: &mut [InventoryRow]) {
    rows.sort_by(|a, b| {
        b.last_active_at
            .cmp(&a.last_active_at)
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// The scanned rows of every root this process has asked about, so a refresh
/// reads only the logs that grew.
#[derive(Default)]
pub struct Inventory {
    cache: Mutex<HashMap<PathBuf, Cached>>,
    scans: AtomicUsize,
}

struct Cached {
    len: u64,
    row: InventoryRow,
}

impl Inventory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every directory under `<root>/.efference/` as a row, most recently
    /// active first. Blocking filesystem work.
    ///
    /// Each log is statted; one whose length matches the cached scan is served
    /// from it, and any other is scanned again. Cached rows whose directory is
    /// gone are dropped. A root with no `.efference` has no rows.
    pub fn refresh(&self, root: &Path) -> Vec<InventoryRow> {
        let base = root.join(crate::session_dir::SESSION_BASE);
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for id in session_dirs(&base) {
            let log = base.join(&id).join(crate::session_dir::LOG_NAME);
            let len = match std::fs::metadata(&log) {
                Ok(meta) if meta.is_file() => meta.len(),
                Ok(_) => {
                    rows.push(InventoryRow::new(&id, root).with_problem("log.jsonl is not a file"));
                    continue;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    rows.push(InventoryRow::new(&id, root).with_problem("no log.jsonl"));
                    continue;
                }
                Err(error) => {
                    rows.push(InventoryRow::new(&id, root).with_problem(unreadable(&error)));
                    continue;
                }
            };
            seen.insert(log.clone());
            if let Some(hit) = self
                .cache
                .lock()
                .unwrap()
                .get(&log)
                .filter(|cached| cached.len == len)
            {
                rows.push(hit.row.clone());
                continue;
            }
            self.scans.fetch_add(1, Ordering::Relaxed);
            match scan_log(&log, &id, root) {
                Ok((row, scanned_len)) => {
                    self.cache.lock().unwrap().insert(
                        log,
                        Cached {
                            len: scanned_len,
                            row: row.clone(),
                        },
                    );
                    rows.push(row);
                }
                // Not cached: a refused read says nothing about the length, so
                // the next refresh tries again.
                Err(error) => {
                    self.cache.lock().unwrap().remove(&log);
                    rows.push(InventoryRow::new(&id, root).with_problem(unreadable(&error)));
                }
            }
        }
        self.cache
            .lock()
            .unwrap()
            .retain(|path, _| !path.starts_with(&base) || seen.contains(path));
        sort_rows(&mut rows);
        rows
    }

    /// How many logs have been scanned (not served from the cache) so far.
    #[cfg(test)]
    fn scans(&self) -> usize {
        self.scans.load(Ordering::Relaxed)
    }
}

fn unreadable(error: &io::Error) -> String {
    format!("cannot read log.jsonl: {error}")
}

/// The names of the directories directly under `base`; empty when `base` is not
/// there.
fn session_dirs(base: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}

/// One log's row and the length it was read at. `Err` only for I/O the
/// filesystem refused; everything wrong with the contents is a row's `problem`.
fn scan_log(log: &Path, id: &str, root: &Path) -> io::Result<(InventoryRow, u64)> {
    let file = File::open(log)?;
    let len = file.metadata()?.len();
    let mut row = InventoryRow::new(id, root);
    if len == 0 {
        row.problem = Some("empty log: no header".into());
        return Ok((row, len));
    }
    let mut reader = BufReader::new(&file);
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line)?;
    let header_len = line.len() as u64;
    let body = trim_newline(&line);
    if let Err(error) = read_header(log, body) {
        row.problem = Some(header_problem(log, &error));
        return Ok((row, len));
    }

    let terminated = line.last() == Some(&b'\n');
    line.clear();
    reader.read_until(b'\n', &mut line)?;
    let first_terminated = line.last() == Some(&b'\n');
    if terminated && !line.is_empty() {
        match parse_entry(&line) {
            Ok(entry) => {
                row.created_at = Some(entry.at.0);
                // `boot::resume` rebuilds the session from the boot preamble;
                // logs that predate it have none and cannot be reopened.
                if !matches!(entry.payload, Payload::Preamble { .. }) {
                    row.problem = Some(
                        "the log does not begin with a boot preamble, so it cannot be resumed"
                            .into(),
                    );
                }
            }
            // The only line after the header, cut short: an append that never
            // finished, so there is nothing to date the session by yet.
            Err(_) if !first_terminated => {}
            Err(error) => {
                row.problem = Some(format!("first entry (line 2) does not parse: {error}"));
                return Ok((row, len));
            }
        }
    }
    drop(reader);
    if row.created_at.is_none() {
        row.problem = Some("no entries after the header".into());
        return Ok((row, len));
    }

    let torn = last_byte(&file, len)? != b'\n';
    let mut tail = Tail::new(&file, header_len, len);
    let mut first = true;
    while let Some(line) = tail.next_line()? {
        let skippable = first && torn;
        first = false;
        match parse_entry(&line) {
            Ok(entry) => {
                row.last_active_at = Some(entry.at.0);
                break;
            }
            Err(_) if skippable => {}
            Err(error) => {
                row.problem = Some(format!("last entry does not parse: {error}"));
                break;
            }
        }
    }

    // One backwards pass finds the last title and the last lifecycle entry, and stops
    // once it has both. A log with no `Resumed` or `Archived` reads to its header.
    let mut lifecycle = None;
    let mut tail = Tail::new(&file, header_len, len);
    while let Some(line) = tail.next_line()? {
        let titled = row.title.is_none() && contains(&line, b"\"Title\"");
        let marked = lifecycle.is_none()
            && (contains(&line, b"{\"Archived\"") || contains(&line, b"{\"Resumed\""));
        if !titled && !marked {
            continue;
        }
        match parse_entry(&line).map(|entry| entry.payload) {
            Ok(Payload::Title(title)) if titled => row.title = Some(title.text),
            Ok(Payload::Archived { source }) if lifecycle.is_none() => {
                lifecycle = Some(false);
                row.archived_by = source;
            }
            Ok(Payload::Resumed(_)) => lifecycle = lifecycle.or(Some(true)),
            _ => {}
        }
        if row.title.is_some() && lifecycle.is_some() {
            break;
        }
    }
    row.interrupted = lifecycle != Some(false);
    Ok((row, len))
}

fn header_problem(log: &Path, error: &crate::persist::LoadError) -> String {
    let text = error.to_string();
    let prefix = format!("{}:", log.display());
    let text = text.strip_prefix(&prefix).unwrap_or(&text).trim();
    format!("header: {text}")
}

fn trim_newline(raw: &[u8]) -> &[u8] {
    let body = raw.strip_suffix(b"\n").unwrap_or(raw);
    body.strip_suffix(b"\r").unwrap_or(body)
}

fn parse_entry(line: &[u8]) -> Result<LogEntry, serde_json::Error> {
    serde_json::from_slice(trim_newline(line))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn last_byte(file: &File, len: u64) -> io::Result<u8> {
    let mut file = file;
    file.seek(SeekFrom::Start(len - 1))?;
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte)?;
    Ok(byte[0])
}

/// The lines of a file from its end back to `floor`, one at a time, reading
/// [`CHUNK`] bytes per step. Blank lines are skipped. Reads stop at the length
/// the scan began with, so an append during the scan is not half-seen.
struct Tail<'a> {
    file: &'a File,
    floor: u64,
    /// Where `pending` starts in the file.
    pos: u64,
    /// Bytes from `pos` up to the line most recently returned.
    pending: Vec<u8>,
}

impl<'a> Tail<'a> {
    fn new(file: &'a File, floor: u64, len: u64) -> Self {
        Self {
            file,
            floor,
            pos: len,
            pending: Vec::new(),
        }
    }

    fn next_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(newline) = self.pending.iter().rposition(|byte| *byte == b'\n') {
                let line = self.pending.split_off(newline + 1);
                self.pending.truncate(newline);
                if trim_newline(&line).is_empty() {
                    continue;
                }
                return Ok(Some(line));
            }
            if self.pos > self.floor {
                let start = self.pos.saturating_sub(CHUNK).max(self.floor);
                let mut chunk = vec![0u8; (self.pos - start) as usize];
                let mut file = self.file;
                file.seek(SeekFrom::Start(start))?;
                file.read_exact(&mut chunk)?;
                chunk.append(&mut self.pending);
                self.pending = chunk;
                self.pos = start;
                continue;
            }
            let line = std::mem::take(&mut self.pending);
            if trim_newline(&line).is_empty() {
                return Ok(None);
            }
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
mod test;
