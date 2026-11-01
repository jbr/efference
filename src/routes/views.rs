//! The JSON readouts served to clients by `/log` (docs/control-api.md), and `answer`, the
//! reply every handler sends a serialized body through. The `/state` readouts are
//! `topic_values.rs`.

use crate::protocol::answers::LocatedToolCall;
use crate::protocol::channel_value;
use crate::protocol::transcript::Handle;
use crate::record::log::LogEntry;
use crate::record::log::Payload;
use crate::routes::entry_view::EntryView;
use crate::routes::entry_view::result_raw;
use crate::routes::http::LogSelect;
use crate::session::Session;
use crate::session::transcript;
use crate::session::transcript::{Grain, Reading};
use serde::Serialize;
use trillium::Conn;

/// Answer `conn` with `body` as the channel carries it ([`channel_value`]): the status,
/// the body's JSON with its keys sorted, and the conn halted so a handler mounted later
/// does not overwrite it.
pub(crate) fn answer(conn: Conn, status: u16, body: impl Serialize) -> Conn {
    conn.with_status(status)
        .with_body(channel_value(body).to_string())
        .halt()
}

impl Session {
    /// The selected window of the log, as a JSON array (docs/control-api.md). Each
    /// element is the entry's [`EntryView::new`] — image `data` omitted, resolvable
    /// through [`Self::image_bytes`], and an oversized result's `content` truncated and
    /// tagged, resolvable through [`Self::entry_json`].
    pub(super) fn log_json(&self, select: &LogSelect) -> String {
        self.with_entries(|entries| {
            let views: Vec<_> = window(entries, select)
                .iter()
                .map(|entry| {
                    EntryView::new(entry)
                        .with_result_raw(result_raw(entry, entries))
                        .with_artifacts(Some(self))
                })
                .collect();
            serde_json::to_string(&views).unwrap_or_else(|_| "[]".into())
        })
    }

    /// The selected window of the log as text (`GET /transcript`, docs/control-api.md),
    /// at `grain`, for the reader `me`; bodies are left behind [`Self::entry_json`]
    /// and the zoom. Same window selection as [`Self::log_json`].
    pub(super) fn transcript(&self, select: &LogSelect, grain: Grain, me: Option<&str>) -> String {
        let (session, held) = self.reading_state();
        let reading = Reading {
            session: session.as_deref(),
            me,
            held: held.as_deref(),
        };
        self.with_entries(|entries| match grain {
            Grain::Audit => transcript::audit::render(window(entries, select)),
            // A tail of the conversation counts its rendered lines, not log entries.
            Grain::Conversation => {
                let bounded = window(
                    entries,
                    &LogSelect {
                        tail: None,
                        ..*select
                    },
                );
                match select.tail {
                    Some(tail) => {
                        transcript::conversation::render_tail(entries, bounded, tail, &reading)
                    }
                    None => transcript::conversation::render(entries, bounded, &reading),
                }
            }
        })
    }

    /// The transcript window the in-session `transcript` tool serves
    /// (docs/tools.md): the same selection as `GET /transcript`, with the
    /// daemon's own argument check, at the conversation grain for the reading
    /// session `me` (docs/control-api.md). The `LogSelect` type stays private to the
    /// daemon, so this is the one public shape.
    pub(crate) fn transcript_window(
        &self,
        since: Option<u64>,
        before: Option<u64>,
        tail: Option<u64>,
        me: &str,
    ) -> Result<String, String> {
        if since.is_some() && tail.is_some() {
            return Err("since and tail are mutually exclusive".into());
        }
        let select = LogSelect {
            since,
            before,
            tail,
        };
        Ok(self.transcript(&select, Grain::Conversation, Some(me)))
    }

    /// One entry in full (`GET /transcript?seq=8.2`, docs/control-api.md), or why there
    /// is no such entry.
    pub(crate) fn transcript_zoom(
        &self,
        handle: Handle,
        me: Option<&str>,
    ) -> Result<String, String> {
        let (session, held) = self.reading_state();
        let reading = Reading {
            session: session.as_deref(),
            me,
            held: held.as_deref(),
        };
        self.with_entries(|entries| transcript::zoom::zoom(entries, handle, &reading))
    }

    /// One entry by `seq`, or `None` if the log has no such position.
    ///
    /// The general form of reference resolution: every reference a windowed client can
    /// hold is ultimately to an entry or to something inside one. Immutably cacheable —
    /// see [`Self::tool_call_json`]. Served as the [`EntryView`], like every
    /// control-channel read of an entry.
    pub(crate) fn entry_json(&self, seq: u64) -> Option<String> {
        self.with_entries(|entries| {
            let entry = entries.iter().find(|e| e.seq == seq)?;
            let view = EntryView::full(entry)
                .with_result_raw(result_raw(entry, entries))
                .with_artifacts(Some(self));
            serde_json::to_string(&view).ok()
        })
    }

    /// The decoded bytes of one logged result image, addressed by its entry's `seq` and
    /// its index within that result — the dereference target for the image references
    /// the views carry (`GET /log/:seq/images/:idx`, docs/control-api.md).
    pub(crate) fn image_bytes(&self, seq: u64, idx: usize) -> Option<(String, Vec<u8>)> {
        use base64::Engine;
        self.with_entries(|entries| {
            let entry = entries.iter().find(|e| e.seq == seq)?;
            let Payload::ToolResult { raw, .. } = &entry.payload else {
                return None;
            };
            let image = raw.images.get(idx)?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&image.data)
                .ok()?;
            Some((image.media_type.clone(), bytes))
        })
    }

    /// One tool call by its `call_id`, with the `seq` of the entry carrying it.
    ///
    /// A call is not its own log entry — it is embedded in an `Assistant` payload — so it
    /// needs its own address, and `call_id` is the one already in the log, in
    /// `tool_output` frames, and in the hand of any client holding an orphaned
    /// `tool_result`. `seq` rides along because a client that resolved a call this way
    /// usually wants the entry it came from next, and it costs nothing to say now.
    ///
    /// Both this and [`Self::entry_json`] are immutably cacheable: a `LogEntry` is
    /// written once and never mutated — triage chooses `wire` at append time from the
    /// verdict rather than flipping it afterward (docs/log-and-projection.md) — so a
    /// returning client never refetches what it has seen.
    pub(super) fn tool_call_json(&self, call_id: &str) -> Option<String> {
        self.with_entries(|entries| {
            let (seq, call) = entries.iter().find_map(|entry| {
                let Payload::Assistant { output, .. } = &entry.payload else {
                    return None;
                };
                let calls = output.tool_calls().ok()?;
                let call = calls.into_iter().find(|c| c.id.0 == call_id)?;
                Some((entry.seq, call))
            })?;
            Some(channel_value(LocatedToolCall { seq, call }).to_string())
        })
    }
}

/// The entries a [`LogSelect`] names, as a sub-slice of the log.
///
/// A slice, never a copy: `since`/`before` are positions in a `seq`-ordered log, so both
/// bounds are binary searches, and `tail` is the last N of what they left. The log is
/// dense today (`seq` == index) but this does not assume it.
fn window<'a>(entries: &'a [LogEntry], select: &LogSelect) -> &'a [LogEntry] {
    let start = match select.since {
        Some(since) => entries.partition_point(|e| e.seq < since),
        None => 0,
    };
    let end = match select.before {
        Some(before) => entries.partition_point(|e| e.seq < before),
        None => entries.len(),
    };
    // `before <= since` names an empty range rather than an error: it is what a client
    // paging backward past the start of the log naturally sends on its last page.
    let selected = &entries[start..end.max(start)];
    match select.tail {
        // Exactly N — or the whole range when it holds fewer, which is the same answer a
        // client gets at the start of a short session.
        Some(n) => &selected[selected.len().saturating_sub(n as usize)..],
        None => selected,
    }
}
