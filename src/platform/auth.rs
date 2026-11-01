//! Root-derived credentials for the TCP bind (docs/web-bind.md).
//!
//! One machine-scoped root secret lives in the secret store (docs/secrets.md) as the
//! `root` item. HKDF with per-purpose context strings derives everything else — device
//! tokens, the VAPID push-signing keypair (docs/web-push.md). Same root in, same
//! credentials out, every boot, so rotation — writing a fresh item over the old —
//! revokes everything derived at once.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::RwLock;

use crate::platform::devices::DeviceId;
use crate::platform::secrets::{SecretError, Secrets};

#[cfg(test)]
mod test;

/// The secret store item holding the root (docs/secrets.md).
pub const ROOT_ITEM: &str = "root";

/// What replacing the root does, in the words every message about it uses.
pub const ROTATION_CONSEQUENCES: &str = "signs out every enrolled device, browsers and \
    scripts alike, and drops every push subscription";

/// The root could not be had. Each names the way out and what it costs.
#[derive(Debug)]
pub enum RootError {
    /// The store answered with an error, or with an empty root. Not absence: minting
    /// over an item that could not be read would silently be a rotation.
    Unreadable(String),
    /// The `[secrets]` commands report no root, and a bind does not mint one through
    /// them: their absent status may be one the tool also gives for other failures, and
    /// a mint over a misread failure would sign out every device (docs/web-bind.md).
    Absent { store: &'static str },
}

impl std::fmt::Display for RootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(why) => write!(
                f,
                "{why}; `efference auth rotate` replaces it, which {ROTATION_CONSEQUENCES}"
            ),
            Self::Absent { store } => write!(
                f,
                "no web root is stored in {store}; `efference auth rotate` creates one, \
                 which {ROTATION_CONSEQUENCES}"
            ),
        }
    }
}

impl std::error::Error for RootError {}

/// The machine-scoped root, HKDF-extracted and ready to expand. Loaded once at
/// daemon boot from the secret store; every derived credential comes off this by a
/// per-purpose context string, so adding a purpose never moves an existing one.
#[derive(Clone)]
pub struct Root(hkdf::Hkdf<Sha256>);

/// The root the running process binds with, for the routes and the push sender
/// that derive from it outside the listener's own state. `None` in every
/// non-serving context — tests, offline verbs, a host with no bind.
static INSTALLED: RwLock<Option<Root>> = RwLock::new(None);

impl Root {
    /// Extract from the stored secret. Whitespace is trimmed so a stray trailing
    /// newline doesn't silently derive a different credential family.
    pub fn new(secret: &str) -> Self {
        Self(hkdf::Hkdf::new(
            Some(b"efference-root v1"),
            secret.trim().as_bytes(),
        ))
    }

    /// The stored root, minting one when there is none, except through the
    /// `[secrets]` commands ([`RootError::Absent`]). A store that fails to answer is
    /// an error rather than a mint: minting over an unreadable item would be a
    /// rotation nobody asked for. So is an empty root, which would derive
    /// credentials anyone could compute.
    pub fn load(secrets: &Secrets) -> Result<Self, RootError> {
        match secrets.get(ROOT_ITEM) {
            Ok(Some(secret)) if secret.trim().is_empty() => Err(RootError::Unreadable(format!(
                "the `{ROOT_ITEM}` item in {} is empty",
                secrets.describe()
            ))),
            Ok(Some(secret)) => Ok(Self::new(&secret)),
            Ok(None) if secrets.runs_commands() => Err(RootError::Absent {
                store: secrets.describe(),
            }),
            Ok(None) => {
                let secret = mint();
                secrets
                    .set(ROOT_ITEM, &secret)
                    .map_err(|error| RootError::Unreadable(error.to_string()))?;
                Ok(Self::new(&secret))
            }
            Err(error) => Err(RootError::Unreadable(error.to_string())),
        }
    }

    /// Store a fresh root over the old one. This is the revocation for everything
    /// derived — [`ROTATION_CONSEQUENCES`]: no device token verifies under the new
    /// root, and `efference auth rotate` deletes the device rows beside it
    /// (docs/web-bind.md). A write rather than a delete, so it needs no `delete`
    /// command under `[secrets]`, where it is also how the first root is made.
    pub fn rotate(secrets: &Secrets) -> Result<(), SecretError> {
        secrets.set(ROOT_ITEM, &mint())
    }

    /// Make `root` the process's root for [`installed`](Self::installed).
    pub fn install(root: Option<Self>) {
        *INSTALLED.write().unwrap() = root;
    }

    /// The root this process binds with, if it binds.
    pub fn installed() -> Option<Self> {
        INSTALLED.read().unwrap().clone()
    }

    /// HKDF-Expand under a context string. The output length is part of the derivation,
    /// so a purpose that changes size is a new context, not a resize.
    fn derive<const N: usize>(&self, context: &[u8]) -> [u8; N] {
        let mut okm = [0u8; N];
        self.0
            .expand(context, &mut okm)
            .expect("okm length is far below the HKDF-SHA256 ceiling");
        okm
    }

    /// The key every device token is minted and verified under (docs/web-bind.md).
    pub fn device_tokens(&self) -> DeviceTokens {
        DeviceTokens(self.derive(b"device-token v1"))
    }

    /// The VAPID signing keypair (docs/web-push.md) — the identity a push service sees
    /// across every message this machine sends. Derived, never stored: each daemon on
    /// the machine re-derives the identical key, so there is no file and no
    /// coordination, and rotating the root strands existing subscriptions the same way
    /// it invalidates every device token.
    ///
    /// P-256 needs a scalar below the group order, and HKDF hands back a uniform 32-byte
    /// string, which exceeds that order with probability ~2⁻³². So the context string
    /// carries an attempt counter and the loop retries until `SecretKey` accepts one:
    /// deterministic per root, so a given machine always takes the same path to the same
    /// key.
    pub fn vapid_key(&self) -> p256::SecretKey {
        let mut attempt = 0u32;
        loop {
            let context = format!("vapid v1 attempt-{attempt}");
            let candidate = self.derive::<32>(context.as_bytes());
            if let Ok(key) = p256::SecretKey::from_slice(&candidate) {
                return key;
            }
            attempt += 1;
        }
    }
}

/// A fresh root: 32 bytes from the OS random source, base64url without padding.
fn mint() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// What every device token starts with: fixed and recognizable, so secret scanners and
/// log redaction can find a pasted token (GitHub's `ghp_` is the convention).
pub const DEVICE_TOKEN_PREFIX: &str = "efference_";

/// The bytes a device token carries: the id, then its HMAC-SHA256 tag.
const DEVICE_TOKEN_BYTES: usize = DeviceId::LEN + 32;

/// Their base64url spelling's length. Forty-eight bytes is a whole number of base64
/// groups, so there is no padding to strip.
const DEVICE_TOKEN_CHARS: usize = DEVICE_TOKEN_BYTES / 3 * 4;

/// The key device tokens are tagged under ([`Root::device_tokens`], docs/web-bind.md).
/// A device token is [`DEVICE_TOKEN_PREFIX`] followed by
/// base64url(`id ‖ HMAC-SHA256(key, id)`), where `id` is the row in the `devices`
/// table the token names (`crate::platform::devices`).
///
/// No salt and no IV: an HMAC under a secret key over a fresh random id is unforgeable
/// as it stands, and a salt guards a low-entropy input against precomputation, which a
/// 128-bit random id is not. So the database holds only public ids, the credential
/// store holds the only secret, and a token is re-derived when it is needed rather than
/// kept anywhere.
#[derive(Clone)]
pub struct DeviceTokens([u8; 32]);

impl DeviceTokens {
    /// The token naming `id`.
    pub fn token(&self, id: &DeviceId) -> String {
        let mut bytes = [0u8; DEVICE_TOKEN_BYTES];
        bytes[..DeviceId::LEN].copy_from_slice(id.as_bytes());
        bytes[DeviceId::LEN..].copy_from_slice(&self.mac(id.as_bytes()).finalize().into_bytes());
        format!("{DEVICE_TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
    }

    /// The id a presented token names, when its tag verifies under this key. The
    /// prefix and length are public and checked plainly; the tag is compared in
    /// constant time (`Mac::verify_slice`). Nothing here reads the device table: a
    /// caller looks the id up only after this answers, so a malformed or forged token
    /// never reaches sqlite.
    pub fn parse(&self, token: &str) -> Option<DeviceId> {
        let encoded = token.strip_prefix(DEVICE_TOKEN_PREFIX)?;
        if encoded.len() != DEVICE_TOKEN_CHARS {
            return None;
        }
        let mut bytes = [0u8; DEVICE_TOKEN_BYTES];
        if URL_SAFE_NO_PAD.decode_slice(encoded, &mut bytes).ok()? != DEVICE_TOKEN_BYTES {
            return None;
        }
        let (id, tag) = bytes.split_at(DeviceId::LEN);
        self.mac(id).verify_slice(tag).ok()?;
        Some(DeviceId::from_bytes(id.try_into().ok()?))
    }

    /// The HMAC over `id`, ready to finalize or verify.
    fn mac(&self, id: &[u8]) -> Hmac<Sha256> {
        Hmac::<Sha256>::new_from_slice(&self.0)
            .expect("hmac accepts any key length")
            .chain_update(id)
    }
}
