//! The update check (docs/update-check.md): whether efference.net names a release
//! newer than this build.
//!
//! Two GETs, `latest.json` and `latest.json.minisig` beside it, through the shared
//! client, so they carry what every request carries and nothing else (`http.rs`). The
//! answer is believed only when the signature checks against the release key compiled
//! in here *and* its trusted comment reads `efference <tag> latest.json` for the tag the
//! document names: a signature covers its trusted comment, so a signature made for
//! another file or another release cannot be presented for this one. Anything short of
//! that is a failed check, never an available version.
//!
//! Who asks, and when, is the caller's: `efference update --check` asks once because it
//! was run, and a host asks on a schedule only when the user configuration says to
//! (`src/daemon/host/update_check.rs`).

use std::time::Duration;

use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use trillium_client::{Client, Status};

/// The release key's public half, minisign's base64. Every published file is signed
/// with its secret half in the release workflow (docs/update-check.md). A binary carries
/// only this key, so one built before a key rotation cannot verify a release signed
/// after it.
pub const RELEASE_PUBLIC_KEY: &str = "RWQVdWuS5d3d6sqA6eXxVuPYvV7/DdkJDgAyDTLmYd3Lz7yssgny/Vq0";

/// Where the newest stable release is described. The signature is this URL plus
/// `.minisig`.
pub const LATEST_URL: &str = "https://efference.net/releases/latest.json";

/// The command that installs the newest release, over an installed one as well:
/// how to update until efference can replace its own binary.
pub const INSTALL_COMMAND: &str = "curl -fsSL https://efference.net/install.sh | sh";

/// This build's version, the one a release is compared against.
pub const INSTALLED: &str = env!("CARGO_PKG_VERSION");

/// The file name every signature over the document carries in its trusted comment.
const DOCUMENT: &str = "latest.json";

/// The most either file may be. Both are a few hundred bytes; the cap only bounds
/// what a misbehaving server can make this read.
const MAX_BYTES: u64 = 16 * 1024;

/// How long one GET may take, so a check against an unresponsive server ends.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A release as `latest.json` describes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// The version as the release's `Cargo.toml` names it: `0.2.0`.
    pub version: String,
    /// The release's tag: `v0.2.0`.
    pub tag: String,
    /// Where the release's files are published.
    pub url: String,
}

impl Release {
    /// Whether this release is newer than `installed`, compared as semantic versions:
    /// a stable release is newer than a prerelease of the same version.
    pub fn is_newer_than(&self, installed: &str) -> Result<bool, String> {
        let release = semver::Version::parse(&self.version)
            .map_err(|e| format!("latest.json names version {:?}: {e}", self.version))?;
        let installed = semver::Version::parse(installed)
            .map_err(|e| format!("this build's version {installed:?}: {e}"))?;
        Ok(release > installed)
    }
}

/// One way of asking: the client the GETs go through, where `latest.json` is, the key
/// its signature must check against, and the version a release is compared with.
#[derive(Clone)]
pub struct UpdateCheck {
    client: Client,
    url: String,
    key: PublicKey,
    installed: String,
}

impl UpdateCheck {
    /// efference.net, through the shared client, against the release key and this
    /// build's version.
    pub fn published() -> Self {
        Self::at(crate::http::client(), LATEST_URL, RELEASE_PUBLIC_KEY)
            .expect("the release key is a minisign public key")
    }

    /// `url` through `client`, checked against `public_key` (minisign's base64),
    /// compared with this build's version.
    pub fn at(client: Client, url: impl Into<String>, public_key: &str) -> Result<Self, String> {
        let key = PublicKey::from_base64(public_key)
            .map_err(|e| format!("{public_key:?} is not a minisign public key: {e}"))?;
        Ok(Self {
            client,
            url: url.into(),
            key,
            installed: INSTALLED.to_string(),
        })
    }

    /// Compare releases with `version` instead of this build's.
    pub fn installed(mut self, version: impl Into<String>) -> Self {
        self.installed = version.into();
        self
    }

    /// The version a release is compared with.
    pub fn installed_version(&self) -> &str {
        &self.installed
    }

    /// The newest release, verified. An unreachable server, a status other than
    /// `200`, a missing or unverifiable signature, or a document that does not
    /// describe a release is an `Err` naming which.
    pub async fn newest(&self) -> Result<Release, String> {
        let document = self.get(&self.url).await?;
        let signature = self.get(&format!("{}.minisig", self.url)).await?;
        let signature =
            String::from_utf8(signature).map_err(|_| format!("{DOCUMENT}.minisig is not text"))?;
        verify(&document, &signature, &self.key)
    }

    /// The newest release when it is newer than the installed version, `None` when
    /// it is not.
    pub async fn newer(&self) -> Result<Option<Release>, String> {
        let release = self.newest().await?;
        Ok(release.is_newer_than(&self.installed)?.then_some(release))
    }

    async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let mut conn = self
            .client
            .get(url)
            .with_timeout(TIMEOUT)
            .await
            .map_err(|e| format!("{url}: {e}"))?;
        match conn.status() {
            Some(Status::Ok) => conn
                .response_body()
                .with_max_len(MAX_BYTES)
                .read_bytes()
                .await
                .map_err(|e| format!("{url}: {e}")),
            Some(status) => Err(format!("{url} answered {status}")),
            None => Err(format!("{url} sent no status")),
        }
    }
}

/// The release `document` describes, when `signature` (the text of a `.minisig`
/// file) checks against `key` and its trusted comment is `efference <tag>
/// latest.json` for the tag the document names.
///
/// The signature is checked before the document is read, so nothing an unsigned
/// document says is acted on, its error message included.
pub fn verify(document: &[u8], signature: &str, key: &PublicKey) -> Result<Release, String> {
    let signature = Signature::decode(signature)
        .map_err(|e| format!("{DOCUMENT}.minisig is not a minisign signature: {e}"))?;
    key.verify(document, &signature, false)
        .map_err(|e| format!("the signature over {DOCUMENT} does not verify: {e}"))?;
    let release: Release = serde_json::from_slice(document)
        .map_err(|e| format!("{DOCUMENT} does not describe a release: {e}"))?;
    let expected = format!("efference {} {DOCUMENT}", release.tag);
    if signature.trusted_comment() != expected {
        return Err(format!(
            "the signature over {DOCUMENT} was made for {:?}, not {expected:?}",
            signature.trusted_comment()
        ));
    }
    Ok(release)
}

#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod test;
