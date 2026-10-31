//! The control-channel serialization of a [`LogEntry`] (docs/control-api.md).
//!
//! Every surface that hands an entry to a client — `GET /log`, `GET /log/:seq`, and a
//! `log` frame's `data` — serializes through [`EntryView`], never the entry directly.
//! The view is the entry verbatim except for two elisions, both resolved by fetching
//! the entry's own address:
//!
//! - **A result image's base64 `data` is omitted, always.** The bytes are a resource
//!   at `GET /log/:seq/images/:idx`, fetched and HTTP-cached separately, so a
//!   multi-megabyte image never rides a log window, a replay, or the SSE stream,
//!   where it would queue every later frame behind it. The image's
//!   `media_type`/`width`/`height` stay in the view so a client can lay out before
//!   fetching.
//! - **A result's `content` past [`MAX_INLINE_RESULT_BYTES`] is truncated, in the
//!   window and frame views only.** [`EntryView::full`] — what `GET /log/:seq`
//!   serves — keeps the whole string. The truncated form is tagged with
//!   `content_elided: {kept_bytes, total_bytes}`, so a client reads "content, cut at
//!   N of M bytes" rather than inferring it from a length.
//!
//! And two additions. A committed assistant entry's view carries `artifacts`, the
//! signed links of its artifact fences (`artifacts.rs`, docs/artifacts.md),
//! when the caller names the session that signs them ([`EntryView::with_artifacts`]).
//! A result's view carries `selection: {total_lines, kept_lines}`
//! — the line arithmetic of the verdict against the whole `content`, computed here
//! from the same [`selection`] the wire and the TUI render through, so a client
//! holding only the truncated prefix still states the count the model's view had.
//! Derived on every read rather than stored: the log stays the record, and a counting
//! bug is fixed by a build rather than baked into a session.
//!
//! Which entry carries those counts is a property of when triage ran
//! (docs/triage.md). A result logged with its own verdict — every log written
//! before capture, and every fixture — carries them on its own entry. A result
//! captured first carries them on the [`Payload::ResultVerdict`] that decides it, so a
//! client updating its slot from one frame gets the same numbers; the result's own
//! view is `Pending` and carries none. That verdict entry holds no `raw` of its own,
//! so the caller resolves the result it names and hands the bytes over
//! ([`EntryView::with_result_raw`]): the view does not reach into the log itself.
//!
//! The log file and the wire projection are untouched — both keep everything inline
//! (docs/log-and-projection.md).
//!
//! Hand-written `Serialize` impls that mirror the derived ones field-for-field, in
//! declaration order. A `serde_json::Value` round-trip would be shorter but re-sorts
//! object keys, breaking the byte-identity between a log frame's `data` and the
//! `GET /log` element (see frames.rs). The mirror obligation is pinned by test:
//! an image-free entry under the threshold serializes byte-identically through both
//! paths, so a field added to [`LogEntry`] or [`RawResult`] without updating this
//! module fails there.

use super::*;
use crate::log::{ContentElision, RawResult, ResultImage, WireState};
use crate::project::selection;
use serde::ser::{SerializeStruct, SerializeStructVariant, Serializer};

/// How much of a tool result's `content` rides a window read or a `log` frame before
/// the rest is left to `GET /log/:seq` (docs/control-api.md).
///
/// 16 KiB, derived from the result-size distribution of this repo's own sessions
/// (2,131 results across 124 logs): the median result is ~1 KB and p90 is ~13 KB, so
/// the threshold sits just past the body of the distribution — ~8% of results elide,
/// and that 8% is ~35% of all result bytes. Halving it to 8 KiB nearly doubles the
/// fetch count for 18 more points of bytes; doubling it to 32 KiB halves the bytes
/// saved. It is the point where a frame's own cost stops being the dominant one:
/// below it, a payload is comparable to the HTTP round-trip that would fetch it, so
/// eliding trades a cheap frame for a dereference and an N+1 read over a normal
/// session.
pub(crate) const MAX_INLINE_RESULT_BYTES: usize = 16 * 1024;

/// A [`LogEntry`] as served to control-channel clients.
pub(crate) struct EntryView<'a> {
    entry: &'a LogEntry,
    /// Whether an oversized result's `content` is truncated. False only for
    /// [`EntryView::full`].
    elide_text: bool,
    /// The bytes of the result a [`Payload::ResultVerdict`] names, when the caller
    /// resolved them — the one thing the verdict entry does not carry itself.
    result_raw: Option<&'a RawResult>,
    /// The session whose key signs the entry's artifact links, when the caller is
    /// serving one; `None` leaves them out.
    session: Option<&'a Session>,
}

impl<'a> EntryView<'a> {
    /// The windowed view — image data omitted, oversized text truncated and tagged.
    /// What `GET /log` elements and `log` frames carry.
    pub(crate) fn new(entry: &'a LogEntry) -> Self {
        Self {
            entry,
            elide_text: true,
            result_raw: None,
            session: None,
        }
    }

    /// The dereference view — image data still omitted (it has its own address), but
    /// `content` whole. What `GET /log/:seq` serves, and the reason a client holding a
    /// truncated result has somewhere to go.
    pub(crate) fn full(entry: &'a LogEntry) -> Self {
        Self {
            entry,
            elide_text: false,
            result_raw: None,
            session: None,
        }
    }

    /// Attach the bytes of the result a [`Payload::ResultVerdict`] names, so the
    /// view can count the lines that verdict keeps (docs/triage.md). The
    /// caller resolves them from the log — a verdict entry holds none of its own —
    /// and `None` leaves the view as the log alone describes it.
    pub(crate) fn with_result_raw(mut self, raw: Option<&'a RawResult>) -> Self {
        self.result_raw = raw;
        self
    }

    /// Carry the entry's artifact links, signed by `session` (docs/artifacts.md):
    /// derived on every read, as `selection` is, so the log stays the only record.
    pub(crate) fn with_artifacts(mut self, session: Option<&'a Session>) -> Self {
        self.session = session;
        self
    }
}

impl Serialize for EntryView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let entry = self.entry;
        let fields =
            4 + usize::from(entry.submission_id.is_some()) + usize::from(entry.source.is_some());
        let mut s = serializer.serialize_struct("LogEntry", fields)?;
        s.serialize_field("seq", &entry.seq)?;
        s.serialize_field("turn", &entry.turn)?;
        s.serialize_field("at", &entry.at)?;
        if let Some(submission_id) = &entry.submission_id {
            s.serialize_field("submission_id", submission_id)?;
        }
        if let Some(source) = &entry.source {
            s.serialize_field("source", source)?;
        }
        let links = self
            .session
            .map_or_default(|session| artifacts::links(session, entry));
        s.serialize_field(
            "payload",
            &PayloadView(&entry.payload, self.elide_text, self.result_raw, &links),
        )?;
        s.end()
    }
}

/// Delegates every variant to the derived serialization except `ToolResult`, the one
/// place a [`ResultImage`] and an oversized `content` live, `ResultVerdict`, whose
/// line counts come from the result it names, and an `Assistant` entry with artifact
/// links.
struct PayloadView<'a>(
    &'a Payload,
    bool,
    Option<&'a RawResult>,
    &'a [artifacts::Link],
);

impl Serialize for PayloadView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Payload::ToolResult { call_id, raw, wire } => {
                let lines = SelectionLines::of(raw, wire);
                let fields = 3 + usize::from(lines.is_some());
                let mut s =
                    serializer.serialize_struct_variant("Payload", 2, "ToolResult", fields)?;
                s.serialize_field("call_id", call_id)?;
                s.serialize_field("raw", &RawResultView(raw, self.1))?;
                s.serialize_field("wire", wire)?;
                if let Some(lines) = lines {
                    s.serialize_field("selection", &lines)?;
                }
                s.end()
            }
            // The line counts land on whichever entry decided the result, so a client
            // reading one frame of a captured result's life sees the same numbers at
            // both ends (docs/triage.md).
            Payload::ResultVerdict { for_result, wire } => {
                let lines = self.2.and_then(|raw| SelectionLines::of(raw, wire));
                let fields = 2 + usize::from(lines.is_some());
                let mut s =
                    serializer.serialize_struct_variant("Payload", 3, "ResultVerdict", fields)?;
                s.serialize_field("for_result", for_result)?;
                s.serialize_field("wire", wire)?;
                if let Some(lines) = lines {
                    s.serialize_field("selection", &lines)?;
                }
                s.end()
            }
            // `output` and `excluded` as the derived form writes them, then the links.
            Payload::Assistant { output, excluded } if !self.3.is_empty() => {
                let fields = 2 + usize::from(excluded.is_some());
                let mut s =
                    serializer.serialize_struct_variant("Payload", 1, "Assistant", fields)?;
                s.serialize_field("output", output)?;
                if let Some(excluded) = excluded {
                    s.serialize_field("excluded", excluded)?;
                }
                s.serialize_field("artifacts", self.3)?;
                s.end()
            }
            other => other.serialize(serializer),
        }
    }
}

/// The bytes of the result a [`Payload::ResultVerdict`] names, or `None` for every
/// other payload — each of those carries everything its own view needs. A `seq` is
/// the result's own address in the log (docs/README.md).
pub(crate) fn result_raw<'e>(entry: &LogEntry, log: &'e [LogEntry]) -> Option<&'e RawResult> {
    let Payload::ResultVerdict { for_result, .. } = &entry.payload else {
        return None;
    };
    match log.get(*for_result as usize)?.payload {
        Payload::ToolResult { ref raw, .. } => Some(raw),
        _ => None,
    }
}

/// The line arithmetic of a `Triaged` verdict against the whole `content`: how many
/// lines the result has, and how many the model's view kept. Present only on a
/// triaged result, so a raw or elided one serializes as before.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SelectionLines {
    pub total_lines: usize,
    pub kept_lines: usize,
}

impl SelectionLines {
    fn of(raw: &RawResult, wire: &WireState) -> Option<Self> {
        let WireState::Triaged(verdict) = wire else {
            return None;
        };
        let d = selection(&raw.content, &verdict.keep);
        Some(Self {
            total_lines: d.shown.len() + d.dropped,
            kept_lines: d.ranges.iter().map(|&(a, b)| b - a + 1).sum(),
        })
    }
}

/// How much of a `content` string a view carries: the whole thing, or a prefix plus
/// the tag saying so.
enum ContentView<'a> {
    Whole(&'a str),
    /// `(prefix, total_bytes)` — the prefix is `content[..kept]` cut at a UTF-8
    /// boundary at or below [`MAX_INLINE_RESULT_BYTES`], so the elided form is still a
    /// valid string rather than a split code point.
    Elided(&'a str, usize),
}

impl<'a> ContentView<'a> {
    fn of(content: &'a str, elide: bool) -> Self {
        if !elide || content.len() <= MAX_INLINE_RESULT_BYTES {
            return Self::Whole(content);
        }
        let mut kept = MAX_INLINE_RESULT_BYTES;
        while kept > 0 && !content.is_char_boundary(kept) {
            kept -= 1;
        }
        Self::Elided(&content[..kept], content.len())
    }
}

struct RawResultView<'a>(&'a RawResult, bool);

impl Serialize for RawResultView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let raw = self.0;
        let content = ContentView::of(&raw.content, self.1);
        // The tagging field is *added* when a result is oversized rather than always
        // present, which is what keeps the frame ≡ window ≡ entry identity true
        // below the threshold (frames.rs): under it, all three views agree field
        // for field.
        let elided = matches!(content, ContentView::Elided(..));
        let fields = 2 + usize::from(elided) + usize::from(!raw.images.is_empty());
        let mut s = serializer.serialize_struct("RawResult", fields)?;
        match content {
            ContentView::Whole(text) => {
                s.serialize_field("content", text)?;
                s.serialize_field("is_error", &raw.is_error)?;
            }
            ContentView::Elided(prefix, total) => {
                s.serialize_field("content", prefix)?;
                s.serialize_field("is_error", &raw.is_error)?;
                s.serialize_field(
                    "content_elided",
                    &ContentElision {
                        kept_bytes: prefix.len(),
                        total_bytes: total,
                    },
                )?;
            }
        }
        if !raw.images.is_empty() {
            s.serialize_field("images", &ImagesView(&raw.images))?;
        }
        s.end()
    }
}

struct ImagesView<'a>(&'a [ResultImage]);

impl Serialize for ImagesView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(ResultImageView))
    }
}

/// The image minus its `data` — the reference shape clients resolve through
/// `GET /log/:seq/images/:idx`.
struct ResultImageView<'a>(&'a ResultImage);

impl Serialize for ResultImageView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("ResultImage", 3)?;
        s.serialize_field("media_type", &self.0.media_type)?;
        s.serialize_field("width", &self.0.width)?;
        s.serialize_field("height", &self.0.height)?;
        s.end()
    }
}

#[cfg(test)]
mod test;
