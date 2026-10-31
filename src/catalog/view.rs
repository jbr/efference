//! The catalog as a route answers it (docs/model-catalog.md): one entry per
//! connection with its status, age, and rows, with recent picks folded in.

use super::{CATALOG_TTL, Snapshot};
use crate::api::{ListedModel, ModelKind};
use crate::log::Provider;
use serde::{Deserialize, Serialize};

/// Where a connection's listing stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// A copy younger than the TTL.
    Fresh,
    /// A copy past the TTL; a refresh is under way or pending.
    Stale,
    /// No copy yet; a refresh is under way or pending.
    Missing,
    /// The last refresh failed and the copy is past the TTL or absent.
    Failed,
    /// The connection has no usable credential (or is disabled), so no read
    /// was attempted.
    Unavailable,
}

/// The role a model is being chosen for. The one place the rule for which models
/// a role may be offered lives, so the host's `?role=` filter and the TUI's
/// pickers cannot differ on it (docs/model-catalog.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRole {
    Main,
    Triage,
    Spotter,
}

impl ModelRole {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "main" => Some(Self::Main),
            "triage" => Some(Self::Triage),
            "spotter" => Some(Self::Spotter),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Triage => "triage",
            Self::Spotter => "spotter",
        }
    }

    /// Whether a model of `kind` can back this role. Main and triage run turns,
    /// which a decision model produces none of; the spotter runs either rung
    /// (docs/permissions.md).
    pub fn accepts(self, kind: ModelKind) -> bool {
        match self {
            Self::Main | Self::Triage => kind.is_text(),
            Self::Spotter => true,
        }
    }

    /// Whether a connection on `provider` can hold any model for this role: a
    /// provider that serves no turns has none to offer a role that runs them.
    pub fn accepts_provider(self, provider: Provider) -> bool {
        match self {
            Self::Main | Self::Triage => provider.serves_inference(),
            Self::Spotter => true,
        }
    }
}

/// One row as a client receives it: the stored row, the connection it belongs
/// to, and the rank of the matching recent pick, folded in at read time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub connection: String,
    #[serde(flatten)]
    pub row: ListedModel,
    /// 1 for the most recently and heavily picked model in the requested
    /// project scope, counted across connections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent: Option<u32>,
}

/// One connection's entry in a catalog answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionCatalog {
    pub connection: String,
    pub provider: Provider,
    pub status: Status,
    /// Seconds since the copy was fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub models: Vec<CatalogModel>,
}

/// A recent pick on one connection: the model and its rank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recent {
    pub model: String,
    pub rank: u32,
}

/// Build one connection's entry. `unavailable` carries the reason a connection
/// cannot be read; such an entry has no stored rows, only recent picks.
///
/// A recent pick the listing does not name (a typed id) becomes a row holding
/// only the connection, the model, the group where the provider derives one from
/// the id, and the rank.
pub fn assemble(
    connection: &str,
    provider: Provider,
    snapshot: Snapshot,
    unavailable: Option<&str>,
    recents: &[Recent],
) -> ConnectionCatalog {
    let (status, error) = match (unavailable, &snapshot.rows, &snapshot.failure) {
        (Some(reason), _, _) => (Status::Unavailable, Some(reason.to_string())),
        (None, None, Some(failure)) => (Status::Failed, Some(failure.error.clone())),
        (None, None, None) => (Status::Missing, None),
        (None, Some(_), failure) => {
            let stale = snapshot.age.is_some_and(|age| age >= CATALOG_TTL.as_secs());
            let error = failure.as_ref().map(|f| f.error.clone());
            match (stale, error) {
                (true, Some(error)) => (Status::Failed, Some(error)),
                (true, None) => (Status::Stale, None),
                (false, error) => (Status::Fresh, error),
            }
        }
    };
    let mut models: Vec<CatalogModel> = snapshot
        .rows
        .unwrap_or_default()
        .into_iter()
        .map(|row| CatalogModel {
            connection: connection.to_string(),
            recent: recents
                .iter()
                .find(|recent| recent.model == row.model)
                .map(|recent| recent.rank),
            row,
        })
        .collect();
    for recent in recents {
        if !models.iter().any(|m| m.row.model == recent.model) {
            models.push(CatalogModel {
                connection: connection.to_string(),
                row: ListedModel {
                    group: provider.group_of(&recent.model),
                    // Nothing was listed to say, so the connection does: a provider
                    // that serves no turns serves only decision models.
                    kind: match provider.serves_inference() {
                        true => ModelKind::Text,
                        false => ModelKind::Decisions,
                    },
                    ..ListedModel::new(&recent.model)
                },
                recent: Some(recent.rank),
            });
        }
    }
    ConnectionCatalog {
        connection: connection.to_string(),
        provider,
        status,
        age: snapshot.age,
        error,
        models,
    }
}

impl ConnectionCatalog {
    /// This entry as `role` may be offered it, or `None` when the connection
    /// holds nothing the role can use. A connection that could serve the role but
    /// lists no model for it stays, empty: a typed id is still possible on it.
    pub fn for_role(mut self, role: ModelRole) -> Option<Self> {
        if !role.accepts_provider(self.provider) {
            return None;
        }
        self.retain_for_role(role);
        Some(self)
    }

    /// Drop the models `role` may not choose, keeping the connection.
    pub fn retain_for_role(&mut self, role: ModelRole) {
        self.models.retain(|model| role.accepts(model.row.kind));
    }
}
