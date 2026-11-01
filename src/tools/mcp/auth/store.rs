//! Where authorized credentials live between runs (docs/mcp-auth.md): one item
//! per server in the secret store (docs/secrets.md), named `mcp:<server>`,
//! holding the [`StoredAuth`] record as JSON.
//!
//! ## Why anything is stored at all
//!
//! The **token** saves the operator from a browser round-trip on every launch.
//! The **registration** saves the *authorization server* from accumulating a
//! new anonymous client record on every launch: dynamic client registration is
//! not idempotent, so a client that re-registers each run litters the server
//! with identical throwaway identities. A Client ID Metadata Document has
//! nothing to store.
//!
//! The item is per user, not per project: an authorization belongs to the
//! operator and the server, and a second checkout of the same project reuses
//! the grant rather than registering again.
//!
//! ## One refresh at a time per server
//!
//! A refresh is a read-modify-write of the item: load the grant, trade its
//! refresh token, save what comes back. Authorization servers rotate refresh
//! tokens for public clients, so two refreshes racing on the same stored token
//! would leave one of them refused, or get the grant revoked as replayed. Each
//! store handle therefore gives out one lock per server ([`RefreshLocks`]), and
//! the handles a host's sessions use share one set, so a host refreshes a
//! server's grant once however many of its sessions find it expired
//! (docs/mcp-auth.md).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::platform::oauth::register::Registration;
use crate::platform::oauth::token::Tokens;
use crate::platform::secrets::Secrets;

/// Everything needed to use — and later refresh — one server's authorization,
/// without redoing discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAuth {
    /// The authorization server that issued this. Checked before the
    /// registration is reused, because a `client_id` is only valid at the
    /// server that minted it.
    pub issuer: String,
    /// The RFC 8707 resource indicator the token was minted for. A refresh must
    /// carry the same one or the new token gets a different audience.
    pub resource: String,
    pub token_endpoint: String,
    pub client_id: String,
    pub tokens: Tokens,
    /// The dynamic registration, when the `client_id` came from one. `None` for
    /// a Client ID Metadata Document, which is self-hosted and needs no record.
    #[serde(default)]
    pub registration: Option<Registration>,
}

/// Load, save, and forget one server's credentials. The interface the auth flow is
/// written against; [`KeychainStore`] is the one implementation.
pub trait TokenStore: Send + Sync {
    fn load(&self, server: &str) -> anyhow::Result<Option<StoredAuth>>;
    fn save(&self, server: &str, auth: &StoredAuth) -> anyhow::Result<()>;
    /// Forget one server's credentials — a revoked grant, or `--reauth`.
    fn clear(&self, server: &str) -> anyhow::Result<()>;
    /// The lock a refresh of `server`'s grant holds from its load to its save
    /// (module docs, "One refresh at a time per server").
    fn refresh_lock(&self, server: &str) -> Arc<tokio::sync::Mutex<()>>;
}

/// One lock per server, for refreshes of its stored grant. Cloning shares the
/// set: a host holds one (`Host::mcp_refresh_locks`) and hands it to the store
/// each of its sessions mounts with.
#[derive(Clone, Default)]
pub struct RefreshLocks(Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>);

impl RefreshLocks {
    /// The lock for `server`, made on first use. One entry per server name ever
    /// refreshed, which a config bounds.
    pub fn lock_for(&self, server: &str) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            self.0
                .lock()
                .unwrap()
                .entry(server.to_string())
                .or_default(),
        )
    }
}

/// The secret store, one item per server.
pub struct KeychainStore {
    secrets: Arc<Secrets>,
    refresh_locks: RefreshLocks,
}

impl KeychainStore {
    /// A handle with a refresh lock set of its own.
    pub fn new(secrets: Arc<Secrets>) -> Self {
        Self {
            secrets,
            refresh_locks: RefreshLocks::default(),
        }
    }

    /// Take refresh locks from `locks`, shared with the other handles holding it.
    pub fn with_refresh_locks(mut self, locks: RefreshLocks) -> Self {
        self.refresh_locks = locks;
        self
    }
}

impl TokenStore for KeychainStore {
    fn load(&self, server: &str) -> anyhow::Result<Option<StoredAuth>> {
        let name = crate::platform::secrets::mcp_name(server);
        let Some(json) = self.secrets.get(&name)? else {
            return Ok(None);
        };
        // A record that does not parse is a hard error, not "no credentials":
        // the latter would quietly restart the browser dance and overwrite it.
        let auth = serde_json::from_str(&json)
            .with_context(|| format!("`{name}` is not readable as stored credentials"))?;
        Ok(Some(auth))
    }

    fn save(&self, server: &str, auth: &StoredAuth) -> anyhow::Result<()> {
        let json = serde_json::to_string(auth)?;
        Ok(self
            .secrets
            .set(&crate::platform::secrets::mcp_name(server), &json)?)
    }

    fn clear(&self, server: &str) -> anyhow::Result<()> {
        Ok(self
            .secrets
            .delete(&crate::platform::secrets::mcp_name(server))?)
    }

    fn refresh_lock(&self, server: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.refresh_locks.lock_for(server)
    }
}

#[cfg(test)]
mod test;
