//! The `[host]` table of the user configuration: the web bind a host started
//! with `efference --host` listens on (docs/web-bind.md).
//!
//! It lives beside the connections because it describes the machine's host, not
//! a shell or a project: a host started by a service manager has no interactive
//! environment to read it from. Flags beside `--host` override each key; a
//! terminal-owned island ignores the table, since it is Unix-socket-only
//! (docs/operating.md).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::api::Error;

/// `[host]`: where a host started with `efference --host` serves the web interface.
/// With no `bind` it serves only its Unix socket. A flag beside `efference --host`
/// overrides each key for that launch.
// `[host]` as written. Every key is optional. The `///` docs are the published user schema's
// hover text (docs/config-schemas.md).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
#[schemars(rename = "Host")]
pub struct HostConfig {
    /// `ADDR:PORT` for the web interface's TCP listener, as in `"127.0.0.1:7878"`. Plain
    /// HTTP is refused on any address but loopback, so another address needs `tls-cert`
    /// and `tls-key`.
    pub bind: Option<String>,
    /// The address browsers open, `https://name[:port]`: the link the host prints, and
    /// the only Host a request may name. Required with TLS; on a loopback bind it
    /// defaults to `http://localhost:<port>`.
    pub public_url: Option<String>,
    /// The PEM certificate chain the bind serves, such as `tailscale cert` writes. A
    /// leading `~/` is your home directory.
    pub tls_cert: Option<PathBuf>,
    /// The PEM private key matching `tls-cert`. A leading `~/` is your home directory.
    pub tls_key: Option<PathBuf>,
}

impl HostConfig {
    /// The table from the user configuration under `home`. A missing file is an
    /// empty table — the host's own launch reports the missing file — and a
    /// leading `~/` in a path is expanded against `home`, since these are
    /// written by hand.
    pub fn load(home: &Path) -> Result<Self, Error> {
        let config = super::InferenceConfig::read_user(home)?.unwrap_or_default();
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
