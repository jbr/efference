//! The signed links to a session's fenced artifacts (docs/artifacts.md): the key a session
//! signs them under, the link a client's entry view carries for each fence, and the HMAC the
//! serving routes check a requested one against (`src/daemon/artifacts.rs`).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;

use super::Session;
use crate::record::artifact::{Fence, Kind, fences};
use crate::record::log::{LogEntry, Payload};

/// The length of a session's signing key.
pub(crate) const KEY_BYTES: usize = 32;

type HmacSha256 = Hmac<Sha256>;

/// A fresh signing key, one per [`Session`].
pub(crate) fn new_key() -> [u8; KEY_BYTES] {
    let mut key = [0; KEY_BYTES];
    rand::rng().fill_bytes(&mut key);
    key
}

/// One artifact's address, as the client entry view carries it (docs/control-api.md).
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema), schemars(rename = "ArtifactLink"))]
pub(crate) struct Link {
    pub(crate) text_block: usize,
    pub(crate) fence: usize,
    pub(crate) kind: Kind,
    /// Root-relative: a web client loads it from the origin it already uses.
    pub(crate) url: String,
    /// The same document on the artifact-only loopback listener, for a local browser;
    /// absent for a session outside a host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) external_url: Option<String>,
}

/// The links of a committed assistant entry's artifacts, in order. Empty for every
/// other entry, for a record-only one, and for a session with no id to address it by.
pub(crate) fn links(session: &Session, entry: &LogEntry) -> Vec<Link> {
    let Payload::Assistant {
        output,
        excluded: None,
    } = &entry.payload
    else {
        return Vec::new();
    };
    let Some(id) = session.id() else {
        return Vec::new();
    };
    let mut links = Vec::new();
    for (text_block, text) in output.text().into_iter().enumerate() {
        for (fence, Fence { kind, .. }) in fences(text).into_iter().enumerate() {
            let tag = tag(session.artifact_key(), id, entry.seq, text_block, fence);
            let url = format!("/artifacts/{id}/{}/{text_block}/{fence}/{tag}", entry.seq);
            let external_url = session
                .artifact_origin()
                .map(|origin| format!("{origin}{url}"));
            links.push(Link {
                text_block,
                fence,
                kind,
                url,
                external_url,
            });
        }
    }
    links
}

pub(crate) fn mac(
    key: &[u8; KEY_BYTES],
    session: &str,
    seq: u64,
    text_block: usize,
    fence: usize,
) -> HmacSha256 {
    let mut mac =
        <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(b"efference-html-artifact-v2\0");
    mac.update(session.as_bytes());
    mac.update(b"\0");
    for coordinate in [seq, text_block as u64, fence as u64] {
        mac.update(&coordinate.to_be_bytes());
    }
    mac
}

fn tag(key: &[u8; KEY_BYTES], session: &str, seq: u64, text_block: usize, fence: usize) -> String {
    URL_SAFE_NO_PAD.encode(
        mac(key, session, seq, text_block, fence)
            .finalize()
            .into_bytes(),
    )
}
