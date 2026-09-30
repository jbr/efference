//! The picker's matching rule (docs/tui.md), as pure functions over a flat node
//! list. No form state, no rendering: the web client implements the same rule in
//! TypeScript and both run `match_cases.json`, so the file is the contract and
//! this module is one reading of it.
//!
//! A node is a connection, a group inside a connection (OpenRouter's authors),
//! or a model. The picker's chips scope the list to a subtree. A node's
//! *slug* is relative to the chips: the whole `connection/model` at the root,
//! `model` under a connection chip, and the model with its `author/` prefix
//! dropped (a leading `~` alias marker kept) under a group chip. A node's *own
//! text* is what it is called inside its parent: a connection's name, a group's
//! name, a model's id without its author.
//!
//! The rule, per whitespace-separated token of the lowercased query (every token
//! must match). A token without `/` is matched against the node's own text, so a
//! row is not found through the names of its ancestors; a token with `/` is
//! matched against the slug. A token's tier is the best of:
//!
//! 0. the token equals the final `/`-segment of the text;
//! 1. the token is a prefix of the text read from the start or from just after
//!    any of `/ - . : ~`;
//! 2. the token is a substring of the text or of the node's `name`;
//! 3. the token is a subsequence of the text.
//!
//! A row's tier is its worst token's. Rows sort by tier, then interior rows
//! before leaves, then (when recents count) recent rank, then list order.

use serde::Deserialize;

/// What a node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Kind {
    Connection,
    Group,
    Model,
}

/// One row of the catalog tree, flat. `model` is the provider-native id (empty
/// for interior nodes); `group` is set on a model whose provider groups by
/// author, and on the group node itself.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(super) struct Node {
    pub(super) kind: Kind,
    pub(super) connection: String,
    #[serde(default)]
    pub(super) group: Option<String>,
    #[serde(default)]
    pub(super) model: String,
    #[serde(default)]
    pub(super) name: Option<String>,
    /// Rank of the matching recent pick, 1 first.
    #[serde(default)]
    pub(super) recent: Option<u32>,
}

/// One segment of the picker's path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Chip {
    Connection(String),
    Group(String),
}

impl Chip {
    pub(super) fn label(&self) -> &str {
        match self {
            Chip::Connection(name) | Chip::Group(name) => name,
        }
    }
}

impl Node {
    /// The node's full slug, the id the fixture names it by: `connection`,
    /// `connection/group`, `connection/model`.
    pub(super) fn id(&self) -> String {
        match (self.kind, &self.group) {
            (Kind::Connection, _) => self.connection.clone(),
            (Kind::Group, Some(group)) => format!("{}/{group}", self.connection),
            (Kind::Group, None) => self.connection.clone(),
            (Kind::Model, _) => format!("{}/{}", self.connection, self.model),
        }
    }

    /// The slug as the picker shows and matches it under `chips`.
    pub(super) fn relative(&self, chips: &[Chip]) -> String {
        match (self.kind, chips.len()) {
            (Kind::Connection, _) => self.connection.clone(),
            (Kind::Group, 0) => self.id(),
            (Kind::Group, _) => self.group.clone().unwrap_or_default(),
            (Kind::Model, 0) => self.id(),
            (Kind::Model, 1) => self.model.clone(),
            (Kind::Model, _) => strip_author(&self.model),
        }
    }

    /// What the node is called inside its parent.
    fn own(&self) -> String {
        match (self.kind, &self.group) {
            (Kind::Connection, _) => self.connection.clone(),
            (Kind::Group, _) => self.group.clone().unwrap_or_default(),
            (Kind::Model, Some(_)) => strip_author(&self.model),
            (Kind::Model, None) => self.model.clone(),
        }
    }

    /// Whether the node sits inside the subtree the chips name.
    pub(super) fn in_scope(&self, chips: &[Chip]) -> bool {
        match chips {
            [] => true,
            [Chip::Connection(connection)] => {
                self.connection == *connection && self.kind != Kind::Connection
            }
            [Chip::Connection(connection), Chip::Group(group)] => {
                self.kind == Kind::Model
                    && self.connection == *connection
                    && self.group.as_deref() == Some(group)
            }
            _ => false,
        }
    }

    fn is_interior(&self) -> bool {
        self.kind != Kind::Model
    }
}

const SEPARATORS: [char; 5] = ['/', '-', '.', ':', '~'];

/// A grouped model id without its `author/` prefix; a leading `~` alias marker
/// stays, since `~claude-latest` and `claude-latest` are different ids.
pub(super) fn strip_author(model: &str) -> String {
    let marker = if model.starts_with('~') { "~" } else { "" };
    match model.split_once('/') {
        Some((_, rest)) => format!("{marker}{rest}"),
        None => model.to_string(),
    }
}

fn subsequence(token: &str, slug: &str) -> bool {
    let mut wanted = token.chars();
    let mut next = wanted.next();
    for c in slug.chars() {
        if Some(c) == next {
            next = wanted.next();
        }
    }
    next.is_none()
}

fn prefix_at_boundary(token: &str, slug: &str) -> bool {
    std::iter::once(0)
        .chain(
            slug.char_indices()
                .filter(|(_, c)| SEPARATORS.contains(c))
                .map(|(i, c)| i + c.len_utf8()),
        )
        .any(|start| slug[start..].starts_with(token))
}

/// One token's tier against a lowercased slug and name.
fn tier(token: &str, slug: &str, name: Option<&str>) -> Option<u8> {
    if slug.rsplit('/').next() == Some(token) {
        Some(0)
    } else if prefix_at_boundary(token, slug) {
        Some(1)
    } else if slug.contains(token) || name.is_some_and(|name| name.contains(token)) {
        Some(2)
    } else if subsequence(token, slug) {
        Some(3)
    } else {
        None
    }
}

/// The indexes of the nodes in scope that match `query`, best first.
/// `recents` is true for the main role, the only role whose picks are recorded.
pub(super) fn rank(nodes: &[Node], chips: &[Chip], query: &str, recents: bool) -> Vec<usize> {
    let query = query.to_lowercase();
    let tokens: Vec<&str> = query.split_whitespace().collect();
    let mut hits: Vec<((u8, bool, u32, usize), usize)> = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.in_scope(chips))
        .filter_map(|(index, node)| {
            let slug = node.relative(chips).to_lowercase();
            let own = node.own().to_lowercase();
            let name = node.name.as_deref().map(str::to_lowercase);
            let worst = tokens
                .iter()
                .map(|token| match token.contains('/') {
                    true => tier(token, &slug, name.as_deref()),
                    false => tier(token, &own, name.as_deref()),
                })
                .try_fold(0u8, |worst, tier| tier.map(|tier| worst.max(tier)))?;
            let recent = match recents {
                true => node.recent.unwrap_or(u32::MAX),
                false => 0,
            };
            Some(((worst, !node.is_interior(), recent, index), index))
        })
        .collect();
    hits.sort();
    hits.into_iter().map(|(_, index)| index).collect()
}

#[cfg(test)]
mod test;
