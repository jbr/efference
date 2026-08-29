//! Root-derived credentials for the TCP bind (docs/web-bind.md).
//!
//! One machine-scoped root secret is injected at daemon start (`EFFERENCE_ROOT_SECRET`,
//! the same op env-file pattern as provider keys) and held only in memory; HKDF with
//! per-purpose context strings derives everything else — the cookie-HMAC key, the TOTP
//! secret, the program bearer, the VAPID push-signing keypair (docs/web-push.md). Same
//! root in, same credentials out, every boot:
//! enrollments and cookies survive restarts with nothing secret on disk, and revocation
//! is root rotation. The only stored state is the burn ledger of accepted TOTP
//! timesteps, which are counters, not secrets.
//!
//! Everything here is pure credential arithmetic; which conns it gates — and which
//! listener is exempt — is the daemon's wiring (docs/web-bind.md).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::Sha256;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

#[cfg(test)]
mod test;

/// The env var carrying the machine-scoped root secret. Required when `serve` is asked
/// to bind TCP; irrelevant otherwise. It is stripped from every tool-child environment
/// at the spawn sites — with it, a child could derive the bearer and drive its own
/// approval gates.
pub const ROOT_ENV: &str = "EFFERENCE_ROOT_SECRET";

/// The machine-scoped root, HKDF-extracted and ready to expand. Constructed once at
/// daemon boot from [`ROOT_ENV`]; every derived credential comes off this by a
/// per-purpose context string, so adding a purpose never moves an existing one.
pub struct Root(hkdf::Hkdf<Sha256>);

impl Root {
    /// Extract from the raw injected secret. Whitespace is trimmed so an env-file
    /// trailing newline doesn't silently derive a different credential family.
    pub fn new(secret: &str) -> Self {
        Self(hkdf::Hkdf::new(
            Some(b"efference-root v1"),
            secret.trim().as_bytes(),
        ))
    }

    /// Read [`ROOT_ENV`], `None` if unset or blank. The caller decides whether absence
    /// is fatal (a TCP bind was requested) or irrelevant (UDS-only serve).
    pub fn from_env() -> Option<Self> {
        std::env::var(ROOT_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| Self::new(&s))
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

    /// The key the session cookie is HMAC-signed with.
    pub fn cookie_key(&self) -> [u8; 32] {
        self.derive(b"cookie-hmac v1")
    }

    /// The TOTP secret — 20 bytes, the RFC 6238 SHA-1 native size, so the base32 in the
    /// `otpauth://` URI round-trips through stock authenticator apps without padding.
    pub fn totp_secret(&self) -> [u8; 20] {
        self.derive(b"totp v1")
    }

    /// The bearer token programs present in `Authorization: Bearer …` — the
    /// non-browser credential (browsers get the cookie because `EventSource` cannot
    /// set a header). Never stored; re-derived on demand.
    pub fn bearer(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.derive::<32>(b"bearer v1"))
    }

    /// The VAPID signing keypair (docs/web-push.md) — the identity a push service sees
    /// across every message this machine sends. Derived, never stored: each daemon on
    /// the machine re-derives the identical key, so there is no file and no
    /// coordination, and rotating the root strands existing subscriptions the same way
    /// it invalidates cookies and the bearer.
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

    /// Constant-time check of a presented bearer: both sides are HMAC'd under a
    /// comparison key and the tags compared as `CtOutput`s, which also erases the
    /// length difference a direct byte comparison would leak.
    pub fn bearer_matches(&self, presented: &str) -> bool {
        let key = self.derive::<32>(b"bearer-compare v1");
        let tag = |s: &str| {
            Hmac::<Sha256>::new_from_slice(&key)
                .expect("hmac accepts any key length")
                .chain_update(s.as_bytes())
                .finalize()
        };
        tag(presented) == tag(&self.bearer())
    }
}

/// The session cookie's name. `__Host-` pins it: `Secure`, no `Domain`, `Path=/` — the
/// browser refuses the cookie from any origin that can't satisfy those, so a plaintext
/// non-loopback origin can't plant one.
pub const COOKIE_NAME: &str = "__Host-efference";

/// How long a minted cookie verifies. Fixed from the signed issued-at — no server-side
/// session store; earlier revocation is root rotation (a mass logout).
pub const COOKIE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Mint a cookie value: `base64url(issued_at_be₈ ‖ HMAC-SHA256(key, issued_at_be₈))`.
/// The cookie carries validity and nothing else — no user id, no scopes; a
/// single-operator "authed or not" bit (docs/web-bind.md).
pub fn mint_cookie(key: &[u8; 32], issued_at: SystemTime) -> String {
    let payload = unix_secs(issued_at).to_be_bytes();
    let tag = Hmac::<Sha256>::new_from_slice(key)
        .expect("hmac accepts any key length")
        .chain_update(payload)
        .finalize()
        .into_bytes();
    let mut raw = Vec::with_capacity(payload.len() + tag.len());
    raw.extend_from_slice(&payload);
    raw.extend_from_slice(&tag);
    URL_SAFE_NO_PAD.encode(raw)
}

/// Whether a presented cookie value verifies: well-formed, authentic (constant-time tag
/// check), issued in the past, and within [`COOKIE_TTL`].
pub fn cookie_is_valid(key: &[u8; 32], value: &str, now: SystemTime) -> bool {
    let Ok(raw) = URL_SAFE_NO_PAD.decode(value) else {
        return false;
    };
    if raw.len() != 8 + 32 {
        return false;
    }
    let (payload, tag) = raw.split_at(8);
    let authentic = Hmac::<Sha256>::new_from_slice(key)
        .expect("hmac accepts any key length")
        .chain_update(payload)
        .verify_slice(tag)
        .is_ok();
    if !authentic {
        return false;
    }
    let issued = u64::from_be_bytes(payload.try_into().expect("split_at(8) yields 8"));
    let now = unix_secs(now);
    now >= issued && now - issued <= COOKIE_TTL.as_secs()
}

/// RFC 6238 parameters: 30 s step, six digits, HMAC-SHA1 — the tuple stock
/// authenticator apps default to, which is what makes the `otpauth://` QR enrollment
/// work with no custom client.
pub const TOTP_STEP_SECS: u64 = 30;

/// Accepted clock skew, in steps, on either side of now.
const TOTP_SKEW: u64 = 1;

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
/// secrets; losing the file re-opens at most the current skew window.
pub struct BurnLedger {
    path: PathBuf,
}

impl BurnLedger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The real location: `~/.efference/totp-burns`. (`BASE_DIR` is repo-relative — a
    /// per-repo ledger would leave a cross-repo replay window, since the TOTP secret is
    /// machine-scoped.) `None` only when the platform reports no home directory.
    pub fn user_scoped() -> Option<Self> {
        std::env::home_dir().map(|home| Self::new(home.join(".efference").join("totp-burns")))
    }

    /// Check-and-burn one accepted step: `Ok(false)` if it was already burned (a
    /// replay), `Ok(true)` if this call burned it. Read-check-rewrite runs under an
    /// exclusive file lock because concurrent daemons share the ledger; the whole file
    /// is rewritten each burn (it holds at most a handful of recent steps).
    pub fn burn(&self, step: u64) -> std::io::Result<bool> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)?;
        file.lock()?; // released when `file` drops
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        let mut burned: Vec<u64> = contents
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect();
        if burned.contains(&step) {
            return Ok(false);
        }
        burned.push(step);
        burned.retain(|&s| s + RETAIN_STEPS >= step);
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        for s in &burned {
            writeln!(file, "{s}")?;
        }
        Ok(true)
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
