//! Shared catalog reads for HTTP and agent resources (docs/model-catalog.md).
use super::Host;
use crate::config::history::Scope;
use crate::inference::ConnectionInventory;
use crate::inference::catalog::{ConnectionCatalog, Recent, assemble};
use std::time::Duration;

impl Host {
    /// Ranked picks shared by catalogue consumers.
    pub(crate) fn catalog_recents(&self, scope: Option<&Scope>) -> Vec<(String, Recent)> {
        let history = self.inference.history();
        let now = crate::config::history::now();
        let picks = match scope {
            Some(scope) => history.recent_all(scope, now, 20),
            None => history.recent_global(now, 20),
        };
        picks
            .into_iter()
            .enumerate()
            .map(|(index, candidate)| {
                (
                    candidate.connection,
                    Recent {
                        model: candidate.model,
                        rank: index as u32 + 1,
                    },
                )
            })
            .collect()
    }
    /// One connection's entry. With `wait`, a copy older than that is refreshed and
    /// awaited; without it a missing or stale copy starts a background refresh and
    /// the stored one is served as it stands.
    pub(crate) async fn catalog_entry(
        &self,
        row: &ConnectionInventory,
        picks: &[(String, Recent)],
        wait: Option<Duration>,
    ) -> ConnectionCatalog {
        let recents: Vec<Recent> = picks
            .iter()
            .filter(|(connection, _)| *connection == row.name)
            .map(|(_, recent)| recent.clone())
            .collect();
        let (unavailable, endpoint) = match self.inference.endpoint(&row.name) {
            Ok((_, endpoint)) => (None, Some(endpoint)),
            Err(error) => (Some(error.to_string()), None),
        };
        let snapshot = match (&endpoint, wait) {
            (Some(endpoint), Some(max_age)) => {
                self.catalog
                    .refresh(&row.name, row.provider, endpoint, max_age)
                    .await
            }
            (Some(endpoint), None) => {
                self.catalog.kick(&row.name, row.provider, endpoint.clone());
                self.catalog
                    .snapshot(&row.name, row.provider, crate::config::history::now())
            }
            (None, _) => {
                self.catalog
                    .snapshot(&row.name, row.provider, crate::config::history::now())
            }
        };
        assemble(
            &row.name,
            row.provider,
            snapshot,
            unavailable.as_deref(),
            &recents,
        )
    }
}
