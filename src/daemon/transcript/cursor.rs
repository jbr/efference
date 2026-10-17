//! A reader's default read: what is new since its last one (docs/tools.md). The cursor
//! is the last seq a reader was shown of a session, held by whoever serves the reader —
//! the host for a session reading another, the mount for an MCP client — and never
//! logged. This file is the arithmetic both share: which window a read covers, the
//! cursor after it, and the header that says what the window is.

use crate::log::Seq;

/// How many entries a reader with no cursor is shown: the tail of the log, since a
/// first read of a long session in full would be most of a context window.
pub(crate) const FIRST_READ_TAIL: u64 = 40;

/// One read, planned: the window to render and what its header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReadPlan {
    /// Entries with `seq >= since`.
    pub(crate) since: Option<u64>,
    /// Entries with `seq < before`: the log's last seq when the read was planned, plus
    /// one, so the cursor names exactly what the window held.
    pub(crate) before: Option<u64>,
    /// The last `tail` entries of the bounded range.
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

    /// The seqs the window holds, first and last, or `None` when it is empty. Seqs
    /// are log positions, so the window is contiguous.
    fn range(&self) -> Option<(Seq, Seq)> {
        let last = self.last?;
        let from = match (self.since, self.tail) {
            (Some(since), _) if since > last => return None,
            (since, Some(tail)) => {
                let floor = since.unwrap_or(0);
                (last + 1).saturating_sub(tail).max(floor)
            }
            (Some(since), None) => since,
            (None, None) => 0,
        };
        (tail_nonzero(self.tail)).then_some((from, last))
    }

    /// The reader's cursor after this read: the end of what it returned, or where it
    /// was when it returned nothing.
    pub(crate) fn cursor_after(&self, cursor: Option<Seq>) -> Option<Seq> {
        match self.range() {
            Some((_, to)) => Some(cursor.map_or(to, |cursor| cursor.max(to))),
            None => cursor,
        }
    }

    /// `rendered` — a window rendered with its own header line first — with the header
    /// replaced by one that says what the window is to this reader. `title` is the
    /// render's own (`transcript of <id>`).
    pub(crate) fn framed(&self, title: &str, rendered: String) -> String {
        let header = match (self.frame, self.range()) {
            (Frame::Asked, _) => return rendered,
            (Frame::New { cursor }, None) => {
                format!("{title}: nothing new since your last read (seq {cursor})")
            }
            (Frame::New { .. }, Some((from, to))) => {
                format!("{title}, new since your last read: {}", seqs(from, to))
            }
            (Frame::First, None) => return rendered,
            (Frame::First, Some((0, to))) => {
                format!("{title}, {} (your first read)", seqs(0, to))
            }
            (Frame::First, Some((from, to))) => format!(
                "{title}, {}: the last {FIRST_READ_TAIL} entries, as this is your first read; \
                 {} come before (since: 0 reads from the start)",
                seqs(from, to),
                seqs(0, from - 1)
            ),
        };
        // The body is everything after the render's header line; a window that held
        // nothing has none.
        let body = match self.range() {
            Some(_) => rendered.split_once('\n').map_or("", |(_, body)| body),
            None => "",
        };
        format!("{header}\n{body}")
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
