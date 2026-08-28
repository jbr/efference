//! Where authorized credentials live between runs (docs/mcp-auth.md).
//!
//! **This is a shim.** [`FileStore`] writes tokens as plaintext JSON, which is
//! not a viable long-term answer and is not meant to be one — the intended
//! replacement is pluggable secret storage, the machine-written counterpart to
//! the human-managed `[env]` references (docs/env.md).
//! [`TokenStore`] exists so that replacement is one new impl rather than a hunt
//! through call sites.
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
//! ## Why the file lives beside the session, not in a keychain
//!
//! A keychain item is bound to the accessing binary, and `cargo build` produces
//! a new one every time — so a keychain-backed store would prompt on every
//! rebuild of a harness under active development. That is an argument for
//! keychain *later*, against a stable installed binary, not for it now.
//!
//! 1Password is a poor fit for this particular data for a different reason:
//! OAuth tokens rotate on every refresh, so this store is written by the
//! machine, frequently. That is the opposite of the `env` passthrough's
//! read-mostly, human-managed secrets, which 1Password suits well. The two
//! halves of "pluggable secret storage" want different backends.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::oauth::register::Registration;
use crate::oauth::token::Tokens;

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

/// Read and write authorized credentials, keyed by the config's server name.
///
/// Keyed by server name rather than by issuer because that is what the caller
/// has in hand at mount time, before any discovery has run. The issuer is
/// checked *after* loading, against what discovery lands on
/// ([`StoredAuth::issuer`]).
pub trait TokenStore {
    fn load(&self, server: &str) -> anyhow::Result<Option<StoredAuth>>;
    fn save(&self, server: &str, auth: &StoredAuth) -> anyhow::Result<()>;
    /// Forget one server's credentials — a revoked grant, or `--reauth`.
    fn clear(&self, server: &str) -> anyhow::Result<()>;
}

/// One JSON file holding every server's credentials.
///
/// Lives in the session base dir, which the harness already denies itself
/// (docs/permissions.md) so a session cannot read its own credentials through
/// the fs tools. That is a real protection but not the reason plaintext is
/// acceptable here — it is acceptable because it is temporary.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            path: base_dir.join("mcp-auth.json"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_all(&self) -> anyhow::Result<std::collections::BTreeMap<String, StoredAuth>> {
        match std::fs::read_to_string(&self.path) {
            Ok(contents) => serde_json::from_str(&contents).with_context(|| {
                format!(
                    "{} is not readable as stored credentials",
                    self.path.display()
                )
            }),
            // No file yet is the normal first-run state, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    fn write_all(
        &self,
        all: &std::collections::BTreeMap<String, StoredAuth>,
    ) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(all)?;
        // Create with owner-only permissions *before* any bytes land, so the
        // tokens are never briefly world-readable.
        write_private(&self.path, contents.as_bytes())
            .with_context(|| format!("writing {}", self.path.display()))
    }
}

impl TokenStore for FileStore {
    fn load(&self, server: &str) -> anyhow::Result<Option<StoredAuth>> {
        Ok(self.read_all()?.remove(server))
    }

    fn save(&self, server: &str, auth: &StoredAuth) -> anyhow::Result<()> {
        let mut all = self.read_all()?;
        all.insert(server.to_string(), auth.clone());
        self.write_all(&all)
    }

    fn clear(&self, server: &str) -> anyhow::Result<()> {
        let mut all = self.read_all()?;
        if all.remove(server).is_none() {
            return Ok(());
        }
        self.write_all(&all)
    }
}

/// Write a file readable only by its owner (`0600`).
///
/// The mode is set at create time rather than with a follow-up `chmod`: between
/// those two calls the tokens would be on disk under the process umask, and a
/// crash in the gap would leave them that way.
#[cfg(unix)]
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

#[cfg(test)]
mod test;
