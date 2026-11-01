//! Detail by recency (docs/tui.md, after docs/web-client.md "Detail by recency";
//! the web client's rules are `ui/transcriptLevels.ts`): how much of each transcript
//! item renders, by its distance from the tail. Recent items render whole, the next
//! stretch trimmed to a glance, older items as a few summary lines. Nothing is
//! dropped — every level opens to the whole on a click.
//!
//! Pure, so the renderer holds none of the rules. A level is a render-time choice:
//! the projection stays a function of the log, and an item's level changes as the
//! tail grows without re-projecting anything.

/// How much of an item renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Level {
    /// Whole, as the item has always rendered.
    #[default]
    Full,
    /// A call's result capped to [`TRIM_ROWS`] under a dimmed last row, with a row
    /// that opens it; long thinking as a short preview.
    Trimmed,
    /// A call as its intent, its signature on one row, and the triage note; long
    /// thinking as one `Thought · N words` row.
    Brief,
}

/// Items this close to the tail render whole.
pub(crate) const RECENT: usize = 6;
/// Items this far from the tail and beyond render brief.
pub(crate) const ANCIENT: usize = 20;
/// Rows of a trimmed result's body shown before it is cut.
pub(crate) const TRIM_ROWS: u16 = 6;
/// Thinking at or under this many words is never folded — about eight wrapped lines,
/// where a fold costs a click and saves nothing. Words rather than lines (thinking is
/// a few long paragraphs) or characters (markdown syntax inflates them).
pub(crate) const THINKING_FLOOR: usize = 120;
/// Rows of a long thinking block shown at the whole level: the box it streamed in,
/// held at its last lines.
pub(crate) const THINKING_BOX_ROWS: u16 = 8;
/// Rows of a long thinking block shown at the trimmed level, from its start.
pub(crate) const THINKING_PREVIEW_ROWS: u16 = 3;

/// An item's level, `from_end` items from the tail (0 is the last). `must_show` is a
/// call that renders whole at any distance: still running or held at a gate, failed,
/// or flagged by the spotter.
pub(crate) fn level_for(from_end: usize, must_show: bool) -> Level {
    if must_show || from_end < RECENT {
        Level::Full
    } else if from_end < ANCIENT {
        Level::Trimmed
    } else {
        Level::Brief
    }
}

pub(crate) fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

#[cfg(test)]
mod test;
