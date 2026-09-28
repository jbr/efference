//! The attachment store — pull-based ingestion of bulky content (docs/inbox.md).
//!
//! An attachment is an **availability** affordance, not a push. A pasted blob, a dragged-in
//! image, a file moved into the store directory: the bytes land on disk and a small notice says
//! what arrived and where — on the turn that carried it (a composer upload, as a
//! [`crate::log::ContentBlock::Attachment`]) or as a [`crate::log::Notification`] (a file
//! written into the store directly). Only the notice enters the log, so a 50 KB paste never
//! pins 50 KB into every subsequent turn's cached prefix (docs/log-and-projection.md). The model
//! decides whether and how to pull it in, using the tools it already has: `read`/`search` for
//! text, `read_image` (with its `region` crop) for an image. When it does pull, the bytes enter
//! as an ordinary tool result and are `Elidable` like any other.
//!
//! The store is a **plain directory**, `.efference/<id>/attachments/`, beside `config.toml`.
//! Being a visible, drag-into-able, fs-watchable folder is the ingestion surface, and nothing
//! deterministic depends on it: the bytes projection must reproduce are the realized image
//! base64'd into the log entry at pull time, so rearranging this directory can neither move
//! committed wire bytes nor cold the cache.
//!
//! This module is the pure half — naming, storing, and describing what arrived. The watcher
//! that notices an arrival and enqueues the notification is `src/daemon/attachments.rs`.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

mod store;
pub use store::Store;

/// The store's directory name inside a session dir (`.efference/<id>/attachments/`).
pub const DIR_NAME: &str = "attachments";

/// The subdirectory a composer's uploads land in (`attachments/composer/`). An upload is
/// announced by the turn that carries it, not by the watcher: the watcher reads only the
/// store's top level, so a file here is never described twice (docs/inbox.md).
pub const COMPOSER_DIR: &str = "composer";

/// How many lines of pasted UTF-8 stay **inline** in the composer. Past this, a paste is
/// stored as an attachment instead — the model can then triage/filter/transform it with the fs
/// tools rather than having it shoved into context. A starting value tuned by feel, not a
/// measured threshold.
pub const INLINE_LINE_LIMIT: usize = 12;

/// How much of a file is read to classify it. Enough to sniff a shape and take a first line;
/// small enough that a multi-megabyte drop costs one page read, not a full load.
const SNIFF_BYTES: usize = 8192;

/// The store directory for a session, given its session storage directory.
pub fn dir_for_session(session_dir: &Path) -> PathBuf {
    session_dir.join(DIR_NAME)
}

/// What an arrived file turned out to be — the triage signals the notification carries.
///
/// The set is deliberately small and prototype-first: enough for the model to decide whether the
/// file is worth pulling *without* re-deriving its content from the notice. Tunable — the right
/// fields are an empirical question about real pastes, not a settled schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// Valid UTF-8: byte size, line count, a sniffed shape, and the first non-blank line.
    Text {
        bytes: u64,
        lines: usize,
        shape: Shape,
        first_line: String,
    },
    /// A decodable image: format, pixel dimensions, byte size.
    Image {
        format: String,
        width: u32,
        height: u32,
        bytes: u64,
    },
    /// Neither — the notice says only what it honestly knows.
    Binary { bytes: u64 },
}

/// A coarse guess at a text attachment's shape, from its first lines. Coarse on purpose: it
/// steers a first move (grep vs read vs ignore) and is never relied on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Shape {
    Json,
    Diff,
    Stacktrace,
    Log,
    Prose,
}

impl Shape {
    fn as_str(self) -> &'static str {
        match self {
            Shape::Json => "JSON",
            Shape::Diff => "a diff",
            Shape::Stacktrace => "a stack trace",
            Shape::Log => "log output",
            Shape::Prose => "prose",
        }
    }
}

/// One file that arrived in the store: where it is, and what it looks like.
///
/// Also the content of a [`crate::log::ContentBlock::Attachment`], which is how a turn carries
/// the files its composer attached — so its serialized form is part of the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arrival {
    /// The file's name inside its store directory — how a client addresses it
    /// (`GET /attachments/:name`) and how a turn references an upload.
    pub name: String,
    /// The path as the model will use it — how the store was addressed, plus the file name.
    pub path: String,
    pub kind: Kind,
}

/// Write `bytes` into the store under a name derived from `suggested`, returning the path.
///
/// The name is made unique by appending `-2`, `-3`, … before the extension rather than
/// overwriting: an attachment the model may not have read yet is not something an arrival with
/// a colliding name gets to replace. Creates the directory if it does not exist — the store is
/// an ordinary folder and a session that never attaches anything never needs one.
pub fn store(dir: &Path, suggested: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let (stem, ext) = split_extension(&sanitize(suggested));
    for n in 1..1000 {
        let name = match (n, ext.as_str()) {
            (1, "") => stem.clone(),
            (1, e) => format!("{stem}.{e}"),
            (n, "") => format!("{stem}-{n}"),
            (n, e) => format!("{stem}-{n}.{e}"),
        };
        let path = dir.join(&name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(bytes)?;
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many attachments with that name",
    ))
}

/// Reduce a suggested name to a single safe path component: no separators, no leading dots, and
/// a fallback when nothing usable is left. A drag-in supplies its own name; a paste supplies one
/// this makes; neither may escape the store directory.
pub(crate) fn sanitize(suggested: &str) -> String {
    let cleaned: String = suggested
        .chars()
        .map(|c| match c {
            '/' | '\\' | '\0' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    if trimmed.is_empty() {
        "attachment".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Split a file name into `(stem, extension)`, extension without the dot and empty when absent.
fn split_extension(name: &str) -> (String, String) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && !ext.contains(' ') => {
            (stem.to_string(), ext.to_string())
        }
        _ => (name.to_string(), String::new()),
    }
}

/// Classify a file that appeared in the store. `display` is the path the notification names —
/// what the model will hand to `read` / `read_image` — and `full` is where the bytes are.
///
/// Fails only when the file cannot be statted or opened; an undecodable image or non-UTF-8 text
/// is [`Kind::Binary`], not an error, because "something arrived that I can't describe" is still
/// worth saying.
pub fn inspect(display: &str, full: &Path) -> io::Result<Arrival> {
    let bytes = std::fs::metadata(full)?.len();
    let kind = classify(full, bytes);
    Ok(Arrival {
        name: full
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: display.to_string(),
        kind,
    })
}

fn classify(full: &Path, bytes: u64) -> Kind {
    // Images first: a PNG is not UTF-8, but a decodable image is a stronger statement than
    // "not text", and dimensions are the signal that decides whether an image is worth a look.
    if let Some((format, width, height)) = image_metadata(full) {
        return Kind::Image {
            format,
            width,
            height,
            bytes,
        };
    }
    let Ok(text) = std::fs::read_to_string(full) else {
        // A large or non-UTF-8 file: sniff the head rather than loading the whole thing.
        return match head_text(full) {
            Some(head) => text_kind(&head, bytes, true),
            None => Kind::Binary { bytes },
        };
    };
    text_kind(&text, bytes, false)
}

/// The head of a file as UTF-8, or `None` when it is not text. A truncation mid-character is
/// dropped rather than lossily replaced — a `` in a notice is noise, and the first line
/// survives either way.
fn head_text(full: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = vec![0u8; SNIFF_BYTES];
    let mut file = std::fs::File::open(full).ok()?;
    let read = file.read(&mut buf).ok()?;
    buf.truncate(read);
    match String::from_utf8(buf) {
        Ok(s) => Some(s),
        Err(e) => {
            let valid = e.utf8_error().valid_up_to();
            // Nothing decodable at the front means it is not text at all.
            (valid > 32).then(|| String::from_utf8_lossy(&e.as_bytes()[..valid]).into_owned())
        }
    }
}

/// Build the text signals. `partial` marks a head-only read, where the line count would be a lie
/// — it is then the count of what was sniffed, and the notice does not claim otherwise.
fn text_kind(text: &str, bytes: u64, partial: bool) -> Kind {
    let lines = if partial {
        0
    } else {
        text.lines().count().max(usize::from(!text.is_empty()))
    };
    Kind::Text {
        bytes,
        lines,
        shape: sniff_shape(text),
        first_line: first_line(text),
    }
}

/// The first non-blank line, trimmed and capped — a fingerprint, not content.
fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let trimmed = line.trim();
    let mut out: String = trimmed.chars().take(120).collect();
    if trimmed.chars().nth(120).is_some() {
        out.push('…');
    }
    out
}

/// Guess a text attachment's shape from its opening lines. Ordered most-specific first; the
/// tests pin the shapes, not the heuristics, so a better sniffer can replace this in place.
fn sniff_shape(text: &str) -> Shape {
    let head: Vec<&str> = text.lines().take(40).collect();
    let first = head
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim())
        .unwrap_or("");
    if (first.starts_with('{') || first.starts_with('[')) && text.trim_end().ends_with(['}', ']']) {
        return Shape::Json;
    }
    if head
        .iter()
        .any(|l| l.starts_with("diff --git") || l.starts_with("@@ ") || l.starts_with("--- "))
    {
        return Shape::Diff;
    }
    if head.iter().any(|l| {
        l.trim_start().starts_with("at ") || l.contains("Traceback (most recent call last)")
    }) || head.iter().any(|l| l.contains("panicked at"))
    {
        return Shape::Stacktrace;
    }
    // Log lines lead with a timestamp or a level word often enough that a majority vote over
    // the head is a better signal than any single line.
    let logish = head
        .iter()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("20")
                || [
                    "ERROR", "WARN", "INFO", "DEBUG", "TRACE", "error:", "warning:",
                ]
                .iter()
                .any(|k| t.starts_with(k))
        })
        .count();
    if !head.is_empty() && logish * 2 >= head.len() {
        return Shape::Log;
    }
    Shape::Prose
}

/// A file's image format and pixel dimensions, or `None` when it does not decode as one.
fn image_metadata(full: &Path) -> Option<(String, u32, u32)> {
    let reader = image::ImageReader::open(full)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let format = reader.format()?;
    let (width, height) = reader.into_dimensions().ok()?;
    Some((format!("{format:?}").to_uppercase(), width, height))
}

/// Bytes as a short human string — the notice states a magnitude, not an exact count.
fn human_bytes(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

/// What an arrival is, as the model reads it: **what arrived and where**, and nothing about what
/// to do with it (docs/inbox.md). The harness takes no stance — the user attached the file, and
/// the user can say what that means — so there is no eager-read default baked in here and no
/// ask-first one either.
///
/// One line, subject first — `shot.png — PNG image, 828×120, 28.7 KB — at <path>` — with a text
/// file's first line under it. A two-line form with the path leading read as boilerplate, and a
/// model looking back at it later could not say whether it had seen the dimensions at all
/// (journal/2026-09-27-composer-attachments.md). The same body is an ambient notice's and, under
/// the harness's `⟢`, a turn's attachment line ([`turn_line`]).
pub fn describe(arrival: &Arrival) -> String {
    let (detail, first) = match &arrival.kind {
        Kind::Text {
            bytes,
            lines,
            shape,
            first_line,
        } => {
            let mut s = "text".to_string();
            if *lines > 0 {
                s.push_str(&format!(", {lines} lines"));
            }
            s.push_str(&format!(
                ", {}, looks like {}",
                human_bytes(*bytes),
                shape.as_str()
            ));
            (s, (!first_line.is_empty()).then(|| first_line.clone()))
        }
        Kind::Image {
            format,
            width,
            height,
            bytes,
        } => (
            format!("{format} image, {width}×{height}, {}", human_bytes(*bytes)),
            None,
        ),
        Kind::Binary { bytes } => (format!("binary, {}", human_bytes(*bytes)), None),
    };
    let mut line = format!("{} — {detail} — at {}", arrival.name, arrival.path);
    if let Some(first) = first {
        line.push_str(&format!("\nfirst line: {first}"));
    }
    line
}

/// How a turn's attachment reads on the wire: the harness's `⟢` note, so the model can tell the
/// harness's report from the operator's words (docs/inbox.md).
pub fn turn_line(arrival: &Arrival) -> String {
    format!("⟢ attached: {}", describe(arrival))
}

/// The notification an arrival enqueues: `(source, body, slot)` for [`crate::daemon`]'s
/// `notify` (docs/inbox.md). The body is [`describe`].
///
/// The slot is the file's path, so a second arrival at the same name supersedes the first
/// notice rather than stacking a second one for the same bytes.
pub fn notification(arrival: &Arrival) -> (String, String, Option<String>) {
    (
        "attachment".to_string(),
        describe(arrival),
        Some(format!("attachment:{}", arrival.path)),
    )
}

/// The media type a file is served under (`GET /attachments/:name`): an image's own, UTF-8 text
/// as `text/plain`, anything else as opaque bytes.
pub fn media_type(full: &Path) -> &'static str {
    if let Some(format) = image::ImageReader::open(full)
        .ok()
        .and_then(|reader| reader.with_guessed_format().ok())
        .and_then(|reader| reader.format())
    {
        return format.to_mime_type();
    }
    match head_text(full) {
        Some(_) => "text/plain; charset=utf-8",
        None if std::fs::metadata(full).is_ok_and(|m| m.len() == 0) => "text/plain; charset=utf-8",
        None => "application/octet-stream",
    }
}

#[cfg(test)]
mod test;
