//! Root-derived credentials for the TCP bind (docs/web-bind.md).
//!
//! One machine-scoped root secret lives in the secret store (docs/secrets.md) as the
//! `root` item: minted from the OS random source the first time a bind needs it, read
//! every boot after, and held only in memory. HKDF with per-purpose context strings
//! derives everything else — the TOTP secret, the key device tokens are tagged under,
//! the VAPID push-signing keypair (docs/web-push.md). Same root in, same credentials
//! out, every boot: enrollments and signed-in devices survive restarts, and rotation —
//! deleting the item, so the next bind mints another — revokes everything derived at
//! once. Nobody supplies or sees the root. What is stored beside it is public: the
//! enrolled devices' ids (`crate::devices`) and the burn ledger of accepted TOTP
//! timesteps, which are counters, not secrets (docs/state-store.md).
//!
//! Everything here is pure credential arithmetic; which conns it gates — and which
//! listener is exempt — is the daemon's wiring (docs/web-bind.md).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::Sha256;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{Duration, Instant, SystemTime};

use crate::devices::DeviceId;
use crate::secrets::{SecretError, Secrets};

#[cfg(test)]
mod test;

/// The secret store item holding the root (docs/secrets.md).
pub const ROOT_ITEM: &str = "root";

/// What replacing the root does, in the words every message about it uses.
pub const ROTATION_CONSEQUENCES: &str = "signs out every enrolled device, browsers and \
    scripts alike, invalidates the authenticator enrollment, and drops every push \
    subscription";

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

    /// The TOTP secret — 20 bytes, the RFC 6238 SHA-1 native size, so the base32 in the
    /// `otpauth://` URI round-trips through stock authenticator apps without padding.
    pub fn totp_secret(&self) -> [u8; 20] {
        self.derive(b"totp v1")
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

/// RFC 6238 parameters: 30 s step, six digits, HMAC-SHA1 — the tuple stock
/// authenticator apps default to, which is what makes the `otpauth://` QR enrollment
/// work with no custom client.
pub const TOTP_STEP_SECS: u64 = 30;

/// Accepted clock skew, in steps, on either side of now.
pub const TOTP_SKEW: u64 = 1;

/// The timestep a moment falls in.
pub fn timestep(now: SystemTime) -> u64 {
    unix_secs(now) / TOTP_STEP_SECS
}

/// The six-digit code for one timestep (RFC 4226 dynamic truncation over HMAC-SHA1).
pub fn totp(secret: &[u8], timestep: u64) -> u32 {
    let digest = Hmac::<Sha1>::new_from_slice(secret)
        .expect("hmac accepts any key length")
        .chain_update(timestep.to_be_bytes())
        .finalize()
        .into_bytes();
    let offset = (digest[19] & 0xf) as usize;
    let bin = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);
    bin % 1_000_000
}

/// A code as the six ASCII digits a person types or a link carries — leading zeros kept.
pub fn format_code(code: u32) -> String {
    format!("{code:06}")
}

/// Which timestep within ±[`TOTP_SKEW`] the presented code matches, if any. Returns the
/// step rather than a bool because acceptance must burn it ([`BurnLedger`]) — the two
/// halves of verification live apart so the ledger I/O never runs on a code that
/// doesn't even match.
pub fn matching_timestep(secret: &[u8], code: &str, now: SystemTime) -> Option<u64> {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let code: u32 = code.parse().ok()?;
    let t = timestep(now);
    (t.saturating_sub(TOTP_SKEW)..=t + TOTP_SKEW).find(|&step| totp(secret, step) == code)
}

/// The `otpauth://` URI a stock authenticator app enrolls from. Carries the secret
/// itself, so it is a host-only surface: rendered as a terminal QR or fetched over the
/// UDS, never served on the TCP listener (docs/web-bind.md).
pub fn otpauth_uri(secret: &[u8], host: &str) -> String {
    format!(
        "otpauth://totp/efference:{host}?secret={}&issuer=efference&algorithm=SHA1&digits=6&period={TOTP_STEP_SECS}",
        data_encoding::BASE32_NOPAD.encode(secret)
    )
}

/// Burned steps older than this many steps behind a new burn are pruned: a code outside
/// the skew window fails [`matching_timestep`] regardless of burn state, so old entries
/// are dead weight, not protection.
const RETAIN_STEPS: u64 = 16;

/// The persisted record of accepted TOTP timesteps — what makes scrollback links and
/// observed codes single-use. User-scoped, not per-session: every daemon under this
/// user shares the derived TOTP secret, so they must share the burns too, or a code
/// accepted by one daemon would replay against another. The steps are counters, not
/// secrets; losing them re-opens at most the current skew window.
pub struct BurnLedger {
    path: PathBuf,
}

/// The ledger's half of the user state database ([`crate::state`],
/// docs/state-store.md). The step is the primary key, so an insert that does
/// nothing is how a replay is recognized.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS totp_burns (
    step INTEGER PRIMARY KEY
);";

impl BurnLedger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The real location: the user state database's `totp_burns` table. It is
    /// user-scoped rather than beside a project's session base because a per-project
    /// ledger would leave a cross-repo replay window, the TOTP secret being
    /// machine-scoped.
    pub fn user_scoped() -> Self {
        Self::new(crate::state::user_scoped())
    }

    fn connect(&self) -> Result<rusqlite::Connection, crate::state::Error> {
        crate::state::open(&self.path, SCHEMA)
    }

    /// Whether `step` has been accepted already. Best-effort, so an unreadable store
    /// reads as nothing burned: a mint that races a burn can hand out a code that then
    /// fails as a replay, which is the outcome [`Self::burn`] exists to allow anyway.
    pub fn is_burned(&self, step: u64) -> bool {
        self.connect().is_ok_and(|db| {
            db.query_row(
                "SELECT 1 FROM totp_burns WHERE step = ?1",
                [step as i64],
                |_| Ok(()),
            )
            .is_ok()
        })
    }

    /// Check-and-burn one accepted step: `Ok(false)` if it was already burned (a
    /// replay), `Ok(true)` if this call burned it. The insert *is* the check: a
    /// primary-key conflict is the other daemon's burn, and nothing else can lose
    /// that race, so concurrent daemons serialize inside the insert, with no lock
    /// spanning a check and a write.
    ///
    /// An error is a step this daemon cannot record, and the caller rejects the code
    /// rather than accept it twice — the fail-closed half of the same reasoning that
    /// lets [`Self::is_burned`] read optimistically.
    pub fn burn(&self, step: u64) -> Result<bool, crate::state::Error> {
        let db = self.connect()?;
        let burned = db.execute(
            "INSERT OR IGNORE INTO totp_burns (step) VALUES (?1)",
            [step as i64],
        )? > 0;
        if burned {
            // Pruned against the step just accepted, and skipped on a replay so a
            // burst of them cannot delete what the window still needs.
            db.execute(
                "DELETE FROM totp_burns WHERE step < ?1 - ?2",
                rusqlite::params![step as i64, RETAIN_STEPS as i64],
            )?;
        }
        Ok(burned)
    }
}

/// First lockout after the free budget is spent; each further failure doubles it.
const LOCKOUT_BASE: Duration = Duration::from_secs(30);

/// Lockout ceiling.
const LOCKOUT_MAX: Duration = Duration::from_secs(15 * 60);

/// Consecutive failures tolerated before the first lockout.
const LOCKOUT_FREE_FAILURES: u32 = 5;

/// The hard rate limit on TOTP verification — six digits is ~20 bits, and this is what
/// makes that arithmetic acceptable. Per-daemon and in-memory: N daemons give an
/// attacker N× the budget, accepted at tailnet posture (docs/web-bind.md).
#[derive(Default)]
pub struct Lockout {
    consecutive_failures: u32,
    locked_until: Option<Instant>,
}

impl Lockout {
    /// Time remaining in an active lockout, or `None` if attempts may proceed.
    pub fn locked_for(&self, now: Instant) -> Option<Duration> {
        self.locked_until
            .and_then(|until| until.checked_duration_since(now))
            .filter(|d| !d.is_zero())
    }

    /// Record a failed attempt. Failures past [`LOCKOUT_FREE_FAILURES`] lock out
    /// verification for a doubling interval, capped at [`LOCKOUT_MAX`].
    pub fn failure(&mut self, now: Instant) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= LOCKOUT_FREE_FAILURES {
            let doublings = (self.consecutive_failures - LOCKOUT_FREE_FAILURES).min(16);
            let lock = LOCKOUT_BASE
                .saturating_mul(1u32 << doublings)
                .min(LOCKOUT_MAX);
            self.locked_until = Some(now + lock);
        }
    }

    /// A successful verification clears the count and any active lockout.
    pub fn success(&mut self) {
        *self = Self::default();
    }
}

/// Seconds since the Unix epoch; a pre-1970 clock reads as 0 rather than panicking.
fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
