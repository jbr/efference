//! A reader's default read: what is new since its last one (docs/tools.md). The cursor
//! is the last seq a reader was shown of a session, held by whoever serves the reader —
//! the host for a session reading another, the mount for an MCP client — and never
//! logged. This file is the arithmetic both share: which window a read covers, the
//! cursor after it, and the header that says what the window is.

use crate::log::Seq;

/// How many conversation lines a reader with no cursor is shown: the tail of the
/// conversation, since a first read of a long session in full would be most of a
/// context window. The window may run longer, as `conversation/tail.rs` extends it back
/// to the call a result belongs to.
pub(crate) const FIRST_READ_TAIL: u64 = 40;

/// One read, planned: the window to render and what its header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReadPlan {
    /// Entries with `seq >= since`.
    pub(crate) since: Option<u64>,
    /// Entries with `seq < before`: the log's last seq when the read was planned, plus
    /// one, so the cursor names exactly what the window held.
    pub(crate) before: Option<u64>,
    /// The last `tail` conversation lines of the bounded range (`GET /transcript`,
    /// docs/control-api.md).
    pub(crate) tail: Option<u64>,
    frame: Frame,
    /// The last seq the window can hold.
    last: Option<Seq>,
}

/// What a read's header says about its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frame {
    /// A window the reader asked for by `since` or `tail`: the render's own header.
    Asked,
    /// Everything after the reader's cursor.
    New { cursor: Seq },
    /// The tail, for a reader with no cursor.
    First,
}

impl ReadPlan {
    /// Plan a read for a reader whose cursor is `cursor`, of a log whose last seq is
    /// `last`. An explicit `since` or `tail` is read as asked; neither is the default
    /// read.
    pub(crate) fn new(
        cursor: Option<Seq>,
        since: Option<u64>,
        tail: Option<u64>,
        last: Option<Seq>,
    ) -> Self {
        let before = last.map(|last| last + 1);
        let (since, tail, frame) = match (since, tail, cursor) {
            (None, None, Some(cursor)) => (Some(cursor + 1), None, Frame::New { cursor }),
            (None, None, None) => (None, Some(FIRST_READ_TAIL), Frame::First),
            (since, tail, _) => (since, tail, Frame::Asked),
        };
        Self {
            since,
            before,
            tail,
            frame,
            last,
        }
    }

    /// The last seq the window holds, or `None` when it is empty. A window always runs
    /// to the log's end: the bounded range's last entry is the last the log held.
    fn end(&self) -> Option<Seq> {
        let last = self.last?;
        if self.since.is_some_and(|since| since > last) {
            return None;
        }
        tail_nonzero(self.tail).then_some(last)
    }

    /// The reader's cursor after this read: the end of what it returned, or where it
    /// was when it returned nothing.
    pub(crate) fn cursor_after(&self, cursor: Option<Seq>) -> Option<Seq> {
        match self.end() {
            Some(to) => Some(cursor.map_or(to, |cursor| cursor.max(to))),
            None => cursor,
        }
    }

    /// `rendered` — a window rendered with its own header line first — with the header
    /// replaced by one that says what the window is to this reader. `title` is the
    /// render's own (`transcript of <id>`). A first read keeps the render's header, which
    /// says what was cut, and adds that it is the first.
    pub(crate) fn framed(&self, title: &str, rendered: String) -> String {
        let (header, body) = rendered.split_once('\n').unwrap_or((&rendered, ""));
        match (self.frame, self.end()) {
            (Frame::Asked | Frame::First, None) => rendered,
            (Frame::New { cursor }, None) => {
                format!("{title}: nothing new since your last read (seq {cursor})\n")
            }
            (Frame::New { cursor }, Some(to)) => format!(
                "{title}, new since your last read: {}\n{body}",
                seqs(cursor + 1, to)
            ),
            (Frame::Asked, Some(_)) => rendered,
            (Frame::First, Some(_)) => format!("{header} (your first read)\n{body}"),
        }
    }
}

/// Whether a `tail` leaves anything to show: `tail: 0` shows nothing.
fn tail_nonzero(tail: Option<u64>) -> bool {
    tail != Some(0)
}

/// `seq 8` or `seq 8–12`.
fn seqs(from: Seq, to: Seq) -> String {
    match from == to {
        true => format!("seq {from}"),
        false => format!("seq {from}–{to}"),
    }
}

#[cfg(test)]
mod test;
