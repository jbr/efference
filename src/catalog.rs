//! Each connection's model listing, held in the user state database
//! (docs/model-catalog.md).
//!
//! A read serves the stored copy with its age. A missing copy, or one older than
//! [`CATALOG_TTL`], is refreshed in the background, at most one read in flight per
//! connection; a caller that needs the answer now awaits the same read
//! ([`Catalog::refresh`]). A failed refresh keeps the old copy and records its
//! error for the status. The rows are the normalized form each provider module
//! produces (`api/provider/row.rs`), so the table holds what a client shows.
//!
//! The table is keyed by connection name, not base URL: Anthropic and OpenAI
//! listings are scoped to the account behind the credential. A stored row whose
//! provider differs from the configured connection's is discarded.

use crate::api::ListedModel;
use crate::log::Provider;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod store;
mod view;
pub use view::{CatalogModel, ConnectionCatalog, ModelRole, Recent, Status, assemble};

/// How long a stored listing stands before a read starts refreshing it. A
/// listing changes when a lab ships a model, not within a day.
pub const CATALOG_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The shortest `max_age` a read may ask for, and the pause after a failed
/// refresh before a background read tries again. Keeps a typed query that
/// matches nothing from re-reading a large listing on every miss.
pub const MAX_AGE_FLOOR: Duration = Duration::from_secs(60);

/// The ceiling on one listing read. A large body is slower than the boot path's
/// metadata reads.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// A stored listing: which provider produced it, when, and the rows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stored {
    pub(crate) provider: String,
    /// Unix seconds.
    pub(crate) fetched_at: i64,
    pub(crate) rows: Vec<ListedModel>,
}

/// The last refresh that failed, until one succeeds.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    /// Unix seconds.
    pub at: i64,
    pub error: String,
}

/// What the catalog holds for one connection at a moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub rows: Option<Vec<ListedModel>>,
    /// Seconds since the copy was fetched.
    pub age: Option<u64>,
    pub failure: Option<Failure>,
}

#[derive(Default)]
struct Slot {
    /// Read from the database on first use.
    loaded: bool,
    copy: Option<Stored>,
    failure: Option<Failure>,
    /// Held for the length of a refresh: the dedupe for concurrent ones.
    refreshing: Arc<tokio::sync::Mutex<()>>,
}

struct Inner {
    path: Option<PathBuf>,
    slots: Mutex<HashMap<String, Slot>>,
}

/// The host's catalog handle. Cheap to clone.
#[derive(Clone)]
pub struct Catalog {
    inner: Arc<Inner>,
}

impl Catalog {
    /// A catalog persisted in the state database at `path`, or held in memory
    /// only when there is none.
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                path,
                slots: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn with_slot<T>(
        &self,
        connection: &str,
        provider: Provider,
        f: impl FnOnce(&mut Slot) -> T,
    ) -> T {
        let mut slots = self.inner.slots.lock().unwrap();
        let slot = slots.entry(connection.to_string()).or_default();
        if !slot.loaded {
            slot.loaded = true;
            slot.copy = self
                .inner
                .path
                .as_deref()
                .and_then(|path| store::load(path, connection));
        }
        if slot
            .copy
            .as_ref()
            .is_some_and(|copy| copy.provider != provider.name())
        {
            slot.copy = None;
            if let Some(path) = self.inner.path.as_deref() {
                store::delete(path, connection);
            }
        }
        f(slot)
    }

    /// What is held for `connection` as configured with `provider`.
    pub fn snapshot(&self, connection: &str, provider: Provider, now: i64) -> Snapshot {
        self.with_slot(connection, provider, |slot| Snapshot {
            rows: slot.copy.as_ref().map(|copy| copy.rows.clone()),
            age: slot
                .copy
                .as_ref()
                .map(|copy| (now - copy.fetched_at).max(0) as u64),
            failure: slot.failure.clone(),
        })
    }

    /// Start a background refresh when the copy is missing or past
    /// [`CATALOG_TTL`], unless one is in flight or one failed within
    /// [`MAX_AGE_FLOOR`]. Returns at once.
    pub fn kick(&self, connection: &str, provider: Provider, endpoint: crate::api::Endpoint) {
        let now = crate::history::now();
        let (wanted, lock) = self.with_slot(connection, provider, |slot| {
            let stale = slot
                .copy
                .as_ref()
                .is_none_or(|copy| (now - copy.fetched_at).max(0) as u64 >= CATALOG_TTL.as_secs());
            let cooling = slot
                .failure
                .as_ref()
                .is_some_and(|f| ((now - f.at).max(0) as u64) < MAX_AGE_FLOOR.as_secs());
            (stale && !cooling, slot.refreshing.clone())
        });
        if !wanted {
            return;
        }
        let Ok(guard) = lock.try_lock_owned() else {
            return;
        };
        let catalog = self.clone();
        let connection = connection.to_string();
        tokio::spawn(async move {
            catalog
                .fetch(&connection, provider, || async {
                    endpoint.catalog_rows().await
                })
                .await;
            drop(guard);
        });
    }

    /// Refresh `connection` and wait for the result when its copy is older than
    /// `max_age` (floored at [`MAX_AGE_FLOOR`]). A refresh already in flight is
    /// awaited rather than repeated. Returns the snapshot afterwards; a failed
    /// read is in [`Snapshot::failure`].
    pub async fn refresh(
        &self,
        connection: &str,
        provider: Provider,
        endpoint: &crate::api::Endpoint,
        max_age: Duration,
    ) -> Snapshot {
        self.refresh_with(connection, provider, max_age, || async {
            endpoint.catalog_rows().await
        })
        .await
    }

    pub(crate) async fn refresh_with<F, Fut>(
        &self,
        connection: &str,
        provider: Provider,
        max_age: Duration,
        read: F,
    ) -> Snapshot
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<ListedModel>, crate::api::Error>>,
    {
        let max_age = max_age.max(MAX_AGE_FLOOR).as_secs();
        let lock = self.with_slot(connection, provider, |slot| slot.refreshing.clone());
        let _guard = lock.lock().await;
        let held = self.snapshot(connection, provider, crate::history::now());
        if held.rows.is_none() || held.age.is_some_and(|age| age > max_age) {
            self.fetch(connection, provider, read).await;
        }
        self.snapshot(connection, provider, crate::history::now())
    }

    /// One listing read, stored on success and recorded on failure. The caller
    /// holds the connection's refresh lock.
    async fn fetch<F, Fut>(&self, connection: &str, provider: Provider, read: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<ListedModel>, crate::api::Error>>,
    {
        let outcome = match tokio::time::timeout(REFRESH_TIMEOUT, read()).await {
            Ok(Ok(rows)) => Ok(rows),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err(format!(
                "the {provider} model listing did not answer within {}s",
                REFRESH_TIMEOUT.as_secs()
            )),
        };
        let now = crate::history::now();
        match outcome {
            Ok(rows) => {
                let stored = Stored {
                    provider: provider.name().to_string(),
                    fetched_at: now,
                    rows,
                };
                if let Some(path) = self.inner.path.as_deref() {
                    store::save(path, connection, &stored);
                }
                self.with_slot(connection, provider, |slot| {
                    slot.copy = Some(stored);
                    slot.failure = None;
                });
            }
            Err(error) => {
                log::warn!("refreshing the {connection} model listing failed: {error}");
                self.with_slot(connection, provider, |slot| {
                    slot.failure = Some(Failure { at: now, error });
                });
            }
        }
    }
}

#[cfg(test)]
mod test;
