//! A file that arrived in a session's attachment store, as the log records it
//! (docs/inbox.md).
//!
//! Naming, storing and classifying files is [`crate::session::attachments`]. These are the shapes
//! a turn or an ambient notice keeps, and the two renderings of them the wire carries:
//! [`describe`] for a notice and [`turn_line`] for a turn.

use serde::{Deserialize, Serialize};

/// What an arrived file turned out to be — the triage signals the notification carries.
///
/// The set is deliberately small: enough for the model to decide whether the
/// file is worth pulling *without* re-deriving its content from the notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    test,
    derive(schemars::JsonSchema),
    schemars(rename = "AttachmentKind")
)]
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
#[cfg_attr(
    test,
    derive(schemars::JsonSchema),
    schemars(rename = "AttachmentShape")
)]
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
/// Also the content of a [`ContentBlock::Attachment`](super::ContentBlock::Attachment), which is how
/// a turn carries the files its composer attached — so its serialized form is part of the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Arrival {
    /// The file's name inside its store directory — how a client addresses it
    /// (`GET /attachments/:name`) and how a turn references an upload.
    pub name: String,
    /// The path as the model will use it — how the store was addressed, plus the file name.
    pub path: String,
    pub kind: Kind,
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
/// file's first line under it: a path-leading form reads as boilerplate. The same body is an
/// ambient notice's and, under the harness's `⟢`, a turn's attachment line ([`turn_line`]).
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
