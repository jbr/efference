//! The session drawer's Policy section as rows (docs/tui.md, docs/permissions.md): the
//! effective policy from the `role` topic and the proposals from the `proposals` topic,
//! merged into one list per section.
//!
//! The merge is the drawer's layout rather than a second copy of the topics. A proposal
//! sits in its committed sort position, flagged, so committing it drops the flag in place
//! without a reflow; the list is built from the store whenever the drawer draws or a
//! key resolves a row, so nothing here goes stale.

use super::topics::Topics;
use crate::policy::ReviewLevel;
use crate::record::log::ConfigChange;

/// One line in a section: a committed grant, or a *proposed* one (rendered tentatively). A
/// proposed entry carries the proposal's `seq`, so the drawer can commit or cancel it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PolicyEntry {
    pub label: String,
    pub proposed: bool,
    pub seq: Option<u64>,
}

/// The Policy section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PolicyView {
    pub scope: Option<String>,
    /// A proposed *replacement* scope, shown tentatively under the committed one:
    /// `(seq, text)`.
    pub scope_proposal: Option<(u64, String)>,
    pub commands: Vec<PolicyEntry>,
    pub paths: Vec<PolicyEntry>,
    pub tools: Vec<PolicyEntry>,
    /// The review-level dial: who reviews a gated call. Read-only here.
    pub review_level: Option<ReviewLevel>,
    /// The position **in force**, when it is not the dial's own value: a `supervisor`
    /// session whose supervisor has ended or was cleared reviews as `spotter_human`, and the
    /// operator reading the drawer is the one who answers (docs/permissions.md). `None`
    /// whenever the two agree.
    pub resolved_review_level: Option<ReviewLevel>,
}

/// Filesystem perms → a compact tag for the section.
fn perm_str(read: bool, write: bool) -> &'static str {
    match (read, write) {
        (_, true) => "rw",
        (true, false) => "r",
        (false, false) => "-",
    }
}

/// A `[tools]` entry's label: its key, then the fields it sets.
fn tool_label(key: &str, enabled: Option<bool>, granted: Option<bool>) -> String {
    let fields: Vec<&str> = [
        enabled.map(|on| if on { "enabled" } else { "off" }),
        granted.map(|on| if on { "granted" } else { "not granted" }),
    ]
    .into_iter()
    .flatten()
    .collect();
    format!("{key}  {}", fields.join(" · "))
}

fn path_label(path: &str, read: bool, write: bool) -> String {
    format!("{path}  {}", perm_str(read, write))
}

impl PolicyView {
    /// The section for the store's current topics, or `None` until the `role` topic has
    /// arrived.
    pub(crate) fn of(topics: &Topics) -> Option<Self> {
        let role = topics.role.as_ref()?;

        // A `[tools]` entry shows its key and the fields it sets, so a carve-out (`off`,
        // `not granted`) reads as one rather than as a grant.
        let mut tools: Vec<PolicyEntry> = role
            .tools
            .iter()
            .map(|(key, settings)| PolicyEntry {
                label: tool_label(key, settings.enabled, settings.granted),
                proposed: false,
                seq: None,
            })
            .collect();
        // A `clear` stance renders as the bare key — the common case, and what the
        // operator wrote — while any other stance is tagged, the same way a path carries
        // its perms: an untagged carve-out would read as a grant.
        let mut commands: Vec<PolicyEntry> = role
            .commands
            .iter()
            .map(|(key, stance)| PolicyEntry {
                label: match stance {
                    crate::config::CommandStance::Clear => key.clone(),
                    other => format!("{key}  {}", other.as_str()),
                },
                proposed: false,
                seq: None,
            })
            .collect();
        let mut paths: Vec<PolicyEntry> = role
            .paths
            .iter()
            .map(|(path, perms)| PolicyEntry {
                label: path_label(path, perms.read, perms.write),
                proposed: false,
                seq: None,
            })
            .collect();

        let mut scope_proposal = None;
        for proposal in &topics.proposals {
            let seq = proposal.seq;
            let proposed = |label: String| PolicyEntry {
                label,
                proposed: true,
                seq: Some(seq),
            };
            match &proposal.change {
                ConfigChange::Command { command } => commands.push(proposed(command.clone())),
                ConfigChange::Path { path, read, write } => {
                    paths.push(proposed(path_label(path, *read, *write)));
                }
                ConfigChange::Tool { name } => {
                    // A logged grant names a namespace as `<namespace>__*` and a tool by wire name.
                    let key =
                        match name.strip_suffix(&crate::record::names::tools::namespace_glob("")) {
                            Some(namespace) => namespace.to_string(),
                            None => crate::config::table_key(name),
                        };
                    // A proposal is shown in its committed section, in sorted position — so
                    // it must be labelled the same way a committed grant is, or it would
                    // sort and read as a different entry than the one it becomes.
                    tools.push(proposed(tool_label(&key, None, Some(true))));
                }
                ConfigChange::Scope { text } => scope_proposal = Some((seq, text.clone())),
            }
        }

        // Sort each section so a proposed entry sits in its committed position (no reflow
        // on commit).
        commands.sort_by(|a, b| a.label.cmp(&b.label));
        paths.sort_by(|a, b| a.label.cmp(&b.label));
        tools.sort_by(|a, b| a.label.cmp(&b.label));

        Some(Self {
            scope: role.scope.clone(),
            scope_proposal,
            commands,
            paths,
            tools,
            review_level: topics.review_level,
            // Reported only when it differs from the dial, which is the only case in which
            // it says something the dial does not (docs/permissions.md).
            resolved_review_level: topics
                .resolved_review_level
                .filter(|resolved| Some(*resolved) != topics.review_level),
        })
    }

    /// The proposed lines in drawer order — the scope first, then each section's — which
    /// are the rows the drawer can commit or cancel.
    pub(crate) fn proposals(&self) -> impl Iterator<Item = u64> + '_ {
        self.scope_proposal
            .as_ref()
            .map(|(seq, _)| *seq)
            .into_iter()
            .chain(
                [&self.commands, &self.paths, &self.tools]
                    .into_iter()
                    .flatten()
                    .filter(|entry| entry.proposed)
                    .filter_map(|entry| entry.seq),
            )
    }
}

#[cfg(test)]
mod test;
