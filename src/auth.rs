//! Root-derived credentials for the TCP bind (docs/web-bind.md).
//!
//! One machine-scoped root secret lives in the secret store (docs/secrets.md) as the
//! `root` item: minted from the OS random source the first time a bind needs it, read
//! every boot after, and held only in memory. HKDF with per-purpose context strings
//! derives everything else — the key device tokens are tagged under, the VAPID
//! push-signing keypair (docs/web-push.md). Same root in, same credentials out, every
//! boot: signed-in devices survive restarts, and rotation — deleting the item, so the
//! next bind mints another — revokes everything derived at once. Nobody supplies or
//! sees the root. What is stored beside it is public: the enrolled devices' ids
//! (`crate::devices`, docs/state-store.md).
//!
//! Everything here is pure credential arithmetic; which conns it gates — and which
//! listener is exempt — is the daemon's wiring (docs/web-bind.md).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::RwLock;

use crate::devices::DeviceId;
use crate::secrets::{SecretError, Secrets};

#[cfg(test)]
mod test;

/// The secret store item holding the root (docs/secrets.md).
pub const ROOT_ITEM: &str = "root";

/// What replacing the root does, in the words every message about it uses.
pub const ROTATION_CONSEQUENCES: &str = "signs out every enrolled device, browsers and \
    scripts alike, and drops every push subscription";

/// The root could not be read. Absence is not this — an absent item is minted — so
/// this is a store that answered with an error, where minting would silently be a
/// rotation. Names the store's complaint and the way out.
#[derive(Debug)]
pub struct RootError(pub String);

impl std::fmt::Display for RootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}; `efference auth rotate` replaces it, which {ROTATION_CONSEQUENCES}",
            self.0
        )
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

    /// The stored root, minting one when there is none. A store that fails to
    /// answer is an error rather than a mint: minting over an unreadable item
    /// would be a rotation nobody asked for.
    pub fn load(secrets: &Secrets) -> Result<Self, RootError> {
        match secrets.get(ROOT_ITEM) {
            Ok(Some(secret)) => Ok(Self::new(&secret)),
            Ok(None) => {
                let mut bytes = [0u8; 32];
                rand::fill(&mut bytes);
                let secret = URL_SAFE_NO_PAD.encode(bytes);
                secrets
                    .set(ROOT_ITEM, &secret)
                    .map_err(|error| RootError(error.to_string()))?;
                Ok(Self::new(&secret))
            }
            Err(error) => Err(RootError(error.to_string())),
        }
    }

    /// Delete the stored root; the next bind mints a fresh one. This is the
    /// revocation for everything derived — [`ROTATION_CONSEQUENCES`]: no device token
    /// verifies under the next root, and `efference auth rotate` deletes the device
    /// rows beside it (docs/web-bind.md).
    pub fn rotate(secrets: &Secrets) -> Result<(), SecretError> {
        secrets.delete(ROOT_ITEM)
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
/// table the token names (`crate::devices`).
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
