//! The picker's rows: the catalog tree flattened, then scoped, filtered, and
//! ordered for the role and the chips (docs/tui.md).
//!
//! The tree has three kinds of node. A connection is interior. A group is
//! interior and exists only where the provider's rows carry a `group`
//! (OpenRouter's authors). A model is a leaf. A typed id is a fourth, synthetic
//! leaf under a connection chip: it is not in any catalog, and is the way to pick
//! a model the listing does not name (docs/model-catalog.md).

use super::matcher::{Kind, Node, rank};
use super::*;
use crate::client::{CatalogModel, CatalogStatus};
use crate::inference::api::Price;

/// Recents the root lists above the connections, main role only.
pub(super) const ROOT_RECENTS: usize = 5;

/// What Enter, Tab, or `/` does with a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Connection(String),
    Group {
        connection: String,
        group: String,
    },
    Model {
        connection: String,
        group: Option<String>,
        model: String,
    },
    /// The operator's own text, offered as a model id.
    Typed {
        connection: String,
        model: String,
    },
}

impl Target {
    pub(super) fn is_interior(&self) -> bool {
        matches!(self, Target::Connection(_) | Target::Group { .. })
    }
}

/// One displayed row, already reduced to the strings and numbers the renderer
/// lays out. `name` is a model's display name when it adds information beyond the
/// id, and a connection's provider.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PickerRow {
    pub(super) target: Target,
    pub(super) id: String,
    pub(super) name: String,
    pub(super) context: Option<u64>,
    pub(super) price: Option<Price>,
    pub(super) created: Option<i64>,
    pub(super) trailer: String,
    /// A connection that cannot be used: drawn dim, still selectable.
    pub(super) dim: bool,
}

/// Per-node display facts, parallel to the node list.
pub(super) enum Extra<'a> {
    Connection {
        provider: String,
        trailer: &'static str,
        dim: bool,
    },
    Group {
        count: usize,
    },
    Model(&'a CatalogModel),
}

/// Whether a display name says something the id does not: the lowercased
/// alphanumerics of each, compared against the id's last segment.
fn informative_name(name: Option<&str>, model: &str) -> String {
    let squash = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let last = model.rsplit('/').next().unwrap_or(model);
    match name {
        Some(name) if squash(name) != squash(last) => name.to_string(),
        _ => String::new(),
    }
}

impl Form {
    /// The picker page as it opens: at the root, nothing typed.
    pub(super) fn picker(role: Role) -> Page {
        Page::Picker {
            role,
            path: Vec::new(),
            query: Line::new(""),
            selected: 0,
            refreshed: Vec::new(),
        }
    }

    pub(super) fn catalog_entry(&self, connection: &str) -> Option<&ConnectionCatalog> {
        self.context
            .catalog
            .iter()
            .find(|entry| entry.connection == connection)
    }

    /// A connection the host reports no usable credential for. Such a
    /// connection is listed and can be chipped: a typed id is still possible, and
    /// preview names the problem.
    pub(super) fn connection_unavailable(&self, connection: &str) -> bool {
        self.connections()
            .iter()
            .any(|row| row.name == connection && !row.available)
            || self
                .catalog_entry(connection)
                .is_some_and(|entry| entry.status == CatalogStatus::Unavailable)
    }

    /// The whole tree in list order: each connection, its groups by name, then
    /// its models as the catalog ordered them — only those `role` may choose
    /// (`ModelRole`, the same rule the host's `?role=` filter applies).
    pub(super) fn tree(&self, role: Role) -> (Vec<Node>, Vec<Extra<'_>>) {
        let role = role.model_role();
        let mut nodes = Vec::new();
        let mut extra = Vec::new();
        for inventory in self
            .connections()
            .iter()
            .filter(|inventory| role.accepts_provider(inventory.provider))
        {
            let entry = self.catalog_entry(&inventory.name);
            let models: Vec<&CatalogModel> = entry
                .map(|entry| entry.models.iter())
                .into_iter()
                .flatten()
                .filter(|model| role.accepts(model.row.kind))
                .collect();
            let (trailer, dim) = match (self.connection_unavailable(&inventory.name), entry) {
                (true, _) => ("unavailable", true),
                (false, Some(entry)) => match entry.status {
                    CatalogStatus::Failed => ("failed", false),
                    CatalogStatus::Missing | CatalogStatus::Stale if models.is_empty() => {
                        ("loading…", false)
                    }
                    _ => ("", false),
                },
                (false, None) => ("", false),
            };
            nodes.push(Node {
                kind: Kind::Connection,
                connection: inventory.name.clone(),
                group: None,
                model: String::new(),
                name: None,
                recent: None,
            });
            extra.push(Extra::Connection {
                provider: inventory.provider.to_string(),
                trailer,
                dim,
            });
            let mut groups: std::collections::BTreeMap<&str, usize> = Default::default();
            for model in &models {
                if let Some(group) = &model.row.group {
                    *groups.entry(group).or_default() += 1;
                }
            }
            for (group, count) in groups {
                nodes.push(Node {
                    kind: Kind::Group,
                    connection: inventory.name.clone(),
                    group: Some(group.to_string()),
                    model: String::new(),
                    name: None,
                    recent: None,
                });
                extra.push(Extra::Group { count });
            }
            for model in models {
                nodes.push(Node {
                    kind: Kind::Model,
                    connection: inventory.name.clone(),
                    group: model.row.group.clone(),
                    model: model.row.model.clone(),
                    name: model.row.name.clone(),
                    recent: model.recent,
                });
                extra.push(Extra::Model(model));
            }
        }
        (nodes, extra)
    }

    /// The node indexes, in order, for an empty query: what the picker shows
    /// before anything is typed.
    fn browse(nodes: &[Node], path: &[Chip], recents: bool) -> Vec<usize> {
        let by_rank = |indexes: &mut Vec<usize>| indexes.sort_by_key(|&i| nodes[i].recent);
        let ranked = |scope: &dyn Fn(&Node) -> bool| -> Vec<usize> {
            let mut indexes: Vec<usize> = match recents {
                true => (0..nodes.len())
                    .filter(|&i| {
                        nodes[i].kind == Kind::Model
                            && nodes[i].recent.is_some()
                            && scope(&nodes[i])
                    })
                    .collect(),
                false => Vec::new(),
            };
            by_rank(&mut indexes);
            indexes
        };
        match path {
            [] => {
                let mut order = ranked(&|_| true);
                order.truncate(ROOT_RECENTS);
                order.extend((0..nodes.len()).filter(|&i| nodes[i].kind == Kind::Connection));
                order
            }
            [Chip::Connection(connection)] => {
                let mut order = ranked(&|node| node.connection == *connection);
                order.truncate(ROOT_RECENTS);
                let grouped = nodes
                    .iter()
                    .any(|node| node.kind == Kind::Group && node.connection == *connection);
                let wanted = match grouped {
                    true => Kind::Group,
                    false => Kind::Model,
                };
                let shown = order.clone();
                order.extend((0..nodes.len()).filter(|&i| {
                    nodes[i].in_scope(path) && nodes[i].kind == wanted && !shown.contains(&i)
                }));
                order
            }
            _ => {
                let mut order = ranked(&|node| node.in_scope(path));
                let shown = order.clone();
                order.extend(
                    (0..nodes.len()).filter(|&i| nodes[i].in_scope(path) && !shown.contains(&i)),
                );
                order
            }
        }
    }

    /// The rows the picker shows now: the tree scoped by the chips, then either
    /// browsed (empty query) or matched and sorted, then the typed-id row.
    pub(super) fn picker_rows(&self) -> Vec<PickerRow> {
        let Page::Picker {
            role, path, query, ..
        } = &self.page
        else {
            return Vec::new();
        };
        let recents = *role == Role::Main;
        let text = query.text();
        let (nodes, extra) = self.tree(*role);
        let order = match text.trim().is_empty() {
            true => Self::browse(&nodes, path, recents),
            false => rank(&nodes, path, &text, recents),
        };
        let mut rows: Vec<PickerRow> = order
            .into_iter()
            .map(|i| {
                let node = &nodes[i];
                let id = node.relative(path);
                let blank = PickerRow {
                    target: Target::Connection(node.connection.clone()),
                    id,
                    name: String::new(),
                    context: None,
                    price: None,
                    created: None,
                    trailer: String::new(),
                    dim: false,
                };
                match &extra[i] {
                    Extra::Connection {
                        provider,
                        trailer,
                        dim,
                    } => PickerRow {
                        name: provider.clone(),
                        trailer: trailer.to_string(),
                        dim: *dim,
                        ..blank
                    },
                    Extra::Group { count } => PickerRow {
                        target: Target::Group {
                            connection: node.connection.clone(),
                            group: node.group.clone().unwrap_or_default(),
                        },
                        trailer: match count {
                            1 => "1 model".to_string(),
                            count => format!("{count} models"),
                        },
                        ..blank
                    },
                    Extra::Model(listed) => PickerRow {
                        target: Target::Model {
                            connection: node.connection.clone(),
                            group: node.group.clone(),
                            model: node.model.clone(),
                        },
                        name: informative_name(node.name.as_deref(), &node.model),
                        context: listed.row.context,
                        price: listed.row.price,
                        created: listed.row.created,
                        trailer: match (recents, node.recent) {
                            (true, Some(_)) => "recent".to_string(),
                            _ => String::new(),
                        },
                        ..blank
                    },
                }
            })
            .collect();
        if let Some(Chip::Connection(connection)) = path.first()
            && !text.is_empty()
            && !text.chars().any(char::is_whitespace)
        {
            // Under a group chip a bare id is the group's: the chip already
            // says the author, and the id the host needs carries it.
            let model = match (path.get(1), text.contains('/')) {
                (Some(Chip::Group(group)), false) => format!("{group}/{text}"),
                _ => text.clone(),
            };
            // A typed id the listing already has is that listed row, facts and
            // all; a second row naming it would offer the same pick twice.
            let listed = rows.iter().any(|row| {
                matches!(&row.target, Target::Model { connection: c, model: m, .. }
                    if c == connection && *m == model)
            });
            if listed {
                return rows;
            }
            rows.push(PickerRow {
                target: Target::Typed {
                    connection: connection.clone(),
                    model: model.clone(),
                },
                id: format!("use `{model}` as model id"),
                name: String::new(),
                context: None,
                price: None,
                created: None,
                trailer: String::new(),
                dim: false,
            });
        }
        rows
    }
}
