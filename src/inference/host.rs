//! The `[host]` table of the user configuration: the web bind a host started
//! with `efference host` listens on (docs/web-bind.md).
//!
//! It lives beside the connections because it describes the machine's host, not
//! a shell or a project: a host started by a service manager has no interactive
//! environment to read it from. Flags on `efference host` override each key; a
//! terminal-owned island ignores the table, since it is Unix-socket-only
//! (docs/operating.md).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::api::Error;

/// `[host]` as written. Every key is optional; with no `bind` the host serves
/// its Unix socket alone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct HostConfig {
    /// `ADDR:PORT` for the TCP listener, the web client's front door.
    pub bind: Option<String>,
    /// The address browsers open, `https://name[:port]`: the printed link and
    /// the Host allowlist. Required with TLS; `http://localhost:<port>` on a
    /// loopback bind otherwise.
    pub public_url: Option<String>,
    /// PEM certificate chain for the bind (what `tailscale cert` writes).
    pub tls_cert: Option<PathBuf>,
    /// PEM private key matching `tls-cert`.
    pub tls_key: Option<PathBuf>,
}

impl HostConfig {
    /// The table from the user configuration under `home`. A missing file is an
    /// empty table — the host's own launch reports the missing file — and a
    /// leading `~/` in a path is expanded against `home`, since these are
    /// written by hand.
    pub fn load(home: &Path) -> Result<Self, Error> {
        let path = home.join(super::CONFIG_RELATIVE_PATH);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(format!("could not read {}: {error}", path.display()).into());
            }
        };
        let config = super::InferenceConfig::from_toml(&text)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(config.host.expand_home(home))
    }

    fn expand_home(mut self, home: &Path) -> Self {
        let expand = |path: PathBuf| match path.strip_prefix("~") {
            Ok(rest) => home.join(rest),
            Err(_) => path,
        };
        self.tls_cert = self.tls_cert.map(expand);
        self.tls_key = self.tls_key.map(expand);
        self
    }
}

#[cfg(test)]
mod test;
