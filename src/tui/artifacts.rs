//! Coordinates and candidate discovery for assistant-authored HTML artifacts.
//!
//! Artifact coordinates follow the visible assistant text blocks and fenced code
//! blocks in provider order. Candidate discovery uses the same closed-fence parser
//! as the resolver so the two sides agree on fence ordinals.

use crate::artifact::closed_html_artifact_fences;
use crate::log::{LogEntry, Payload};

/// One zero-based artifact location within a committed assistant entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Coordinate {
    pub seq: u64,
    pub text_block: usize,
    pub fence: usize,
}

/// The closed `html-artifact` fences in a committed assistant entry, in provider order.
pub(crate) fn candidates(entry: &LogEntry) -> Vec<Coordinate> {
    let Payload::Assistant {
        output,
        excluded: None,
    } = &entry.payload
    else {
        return Vec::new();
    };

    output
        .text()
        .into_iter()
        .enumerate()
        .flat_map(|(text_block, text)| {
            closed_html_artifact_fences(text)
                .into_iter()
                .map(|fence| Coordinate {
                    seq: entry.seq,
                    text_block,
                    fence: fence.ordinal,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod test;
