//! The control-channel serialization of a [`LogEntry`] (docs/control-api.md).
//!
//! Every surface that hands an entry to a client — `GET /log`, `GET /log/:seq`, and a
//! `log` frame's `data` — serializes through [`EntryView`], never the entry directly.
//! The view is the entry verbatim except that a result image's base64 `data` is
//! omitted: the bytes are a resource at `GET /log/:seq/images/:idx`, fetched and
//! HTTP-cached separately, so a multi-megabyte image never rides a log window, a
//! replay, or the SSE stream, where it would queue every later frame behind it. The
//! image's `media_type`/`width`/`height` stay in the view so a client can lay out
//! before fetching. The log file and the wire projection are untouched — both keep
//! the bytes inline (docs/log-and-projection.md).
//!
//! Hand-written `Serialize` impls that mirror the derived ones field-for-field, in
//! declaration order. A `serde_json::Value` round-trip would be shorter but re-sorts
//! object keys, breaking the byte-identity between a log frame's `data` and the
//! `GET /log` element (see frames.rs). The mirror obligation is pinned by test:
//! an image-free entry serializes byte-identically through both paths, so a field
//! added to [`LogEntry`] or [`RawResult`] without updating this module fails there.

use super::*;
use crate::log::{RawResult, ResultImage};
use serde::ser::{SerializeStruct, SerializeStructVariant, Serializer};

/// A [`LogEntry`] as served to control-channel clients.
pub(crate) struct EntryView<'a>(pub(crate) &'a LogEntry);

impl Serialize for EntryView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let entry = self.0;
        let fields = 4 + usize::from(entry.submission_id.is_some());
        let mut s = serializer.serialize_struct("LogEntry", fields)?;
        s.serialize_field("seq", &entry.seq)?;
        s.serialize_field("turn", &entry.turn)?;
        s.serialize_field("at", &entry.at)?;
        if let Some(submission_id) = &entry.submission_id {
            s.serialize_field("submission_id", submission_id)?;
        }
        s.serialize_field("payload", &PayloadView(&entry.payload))?;
        s.end()
    }
}

/// Delegates every variant to the derived serialization except `ToolResult`, the one
/// place a [`ResultImage`] lives.
struct PayloadView<'a>(&'a Payload);

impl Serialize for PayloadView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Payload::ToolResult { call_id, raw, wire } => {
                let mut s = serializer.serialize_struct_variant("Payload", 2, "ToolResult", 3)?;
                s.serialize_field("call_id", call_id)?;
                s.serialize_field("raw", &RawResultView(raw))?;
                s.serialize_field("wire", wire)?;
                s.end()
            }
            other => other.serialize(serializer),
        }
    }
}

struct RawResultView<'a>(&'a RawResult);

impl Serialize for RawResultView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let raw = self.0;
        let fields = 2 + usize::from(!raw.images.is_empty());
        let mut s = serializer.serialize_struct("RawResult", fields)?;
        s.serialize_field("content", &raw.content)?;
        s.serialize_field("is_error", &raw.is_error)?;
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
