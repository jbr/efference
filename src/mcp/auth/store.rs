//! Where authorized credentials live between runs (docs/mcp-auth.md): one item
//! per server in the secret store (docs/secrets.md), named `mcp:<server>`,
//! holding the [`StoredAuth`] record as JSON.
//!
//! ## Why anything is stored at all
//!
//! Two things, and only one of them is the obvious one.
//!
//! The **token** saves the operator from a browser round-trip on every launch.
//! The **registration** saves the *authorization server* from accumulating a
//! new anonymous client record on every launch: dynamic client registration is
//! not idempotent, so a client that re-registers each run litters the server
//! with identical throwaway identities. A Client ID Metadata Document has
//! nothing to store, which is one of its quieter advantages.
//!
//! The item is per user, not per project: an authorization belongs to the
//! operator and the server, and a second checkout of the same project reuses
//! the grant rather than registering again.

use std::sync::Arc;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::oauth::register::Registration;
use crate::oauth::token::Tokens;
use crate::secrets::Secrets;

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

/// Load, save, and forget one server's credentials. The seam the auth flow is
/// written against; [`KeychainStore`] is the one implementation.
pub trait TokenStore: Send + Sync {
    fn load(&self, server: &str) -> anyhow::Result<Option<StoredAuth>>;
    fn save(&self, server: &str, auth: &StoredAuth) -> anyhow::Result<()>;
    /// Forget one server's credentials — a revoked grant, or `--reauth`.
    fn clear(&self, server: &str) -> anyhow::Result<()>;
}

/// The secret store, one item per server.
pub struct KeychainStore {
    secrets: Arc<Secrets>,
}

impl KeychainStore {
    pub fn new(secrets: Arc<Secrets>) -> Self {
        Self { secrets }
    }
}

impl TokenStore for KeychainStore {
    fn load(&self, server: &str) -> anyhow::Result<Option<StoredAuth>> {
        let name = crate::secrets::mcp_name(server);
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
        Ok(self.secrets.set(&crate::secrets::mcp_name(server), &json)?)
    }

    fn clear(&self, server: &str) -> anyhow::Result<()> {
        Ok(self.secrets.delete(&crate::secrets::mcp_name(server))?)
    }
}

#[cfg(test)]
mod test;
