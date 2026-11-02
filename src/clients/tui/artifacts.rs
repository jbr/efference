//! Artifact fences as the TUI receives them (docs/artifacts.md): the entry view of a
//! committed assistant entry carries each artifact fence's coordinates and kind, and
//! the TUI draws a notice where the web client would render the page.

use crate::record::artifact::Kind;

/// One artifact's position within a committed assistant entry, zero-based. Ordered
/// by entry, then text block, then fence: the order the entry's notices are drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Coordinate {
    pub seq: u64,
    pub text_block: usize,
    pub fence: usize,
}

/// The linked fences in a `log` frame's entry view, by coordinate with their kind.
/// A fence whose kind the view does not name is not an artifact the TUI knows.
pub(crate) fn linked_fences(entry_view: &serde_json::Value) -> Vec<(Coordinate, Kind)> {
    let Some(seq) = entry_view["seq"].as_u64() else {
        return Vec::new();
    };
    let Some(links) = entry_view["payload"]["Assistant"]["artifacts"].as_array() else {
        return Vec::new();
    };
    links
        .iter()
        .filter_map(|link| {
            let coordinate = Coordinate {
                seq,
                text_block: usize::try_from(link["text_block"].as_u64()?).ok()?,
                fence: usize::try_from(link["fence"].as_u64()?).ok()?,
            };
            let kind = match link["kind"].as_str()? {
                "html" => Kind::Html,
                "mermaid" => Kind::Mermaid,
                _ => return None,
            };
            Some((coordinate, kind))
        })
        .collect()
}

#[cfg(test)]
mod test;
