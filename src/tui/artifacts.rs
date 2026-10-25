//! Artifact links as the TUI receives them (docs/artifacts.md): the entry view of a
//! committed assistant entry carries each `html-artifact` fence's signed links, and
//! the TUI shows the loopback one, which a local browser can open.

/// One artifact's position within a committed assistant entry, zero-based. Ordered
/// by entry, then text block, then fence: the order the entry's links are drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Coordinate {
    pub seq: u64,
    pub text_block: usize,
    pub fence: usize,
}

/// The loopback links in a `log` frame's entry view, by coordinate. A link without
/// one — a session outside a host — has nothing a local browser can open.
pub(crate) fn external_links(entry_view: &serde_json::Value) -> Vec<(Coordinate, String)> {
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
            Some((coordinate, link["external_url"].as_str()?.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod test;
