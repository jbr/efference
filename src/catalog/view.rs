//! The catalog as a route answers it (docs/model-catalog.md): one entry per
//! connection with its status, age, and rows, with recent picks folded in.

use super::{CATALOG_TTL, Snapshot};
use crate::api::{ListedModel, Provider};
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
    unavailable: Option<String>,
    recents: &[Recent],
) -> ConnectionCatalog {
    let (status, error) = match (&unavailable, &snapshot.rows, &snapshot.failure) {
        (Some(reason), _, _) => (Status::Unavailable, Some(reason.clone())),
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
