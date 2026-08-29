use super::*;
use std::time::UNIX_EPOCH;

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

/// A throwaway ledger path under the OS temp dir (same create-or-retry entropy idiom as
/// the scratch dir; no tempfile dependency).
fn scratch_ledger() -> BurnLedger {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let n = RandomState::new().build_hasher().finish();
    BurnLedger::new(std::env::temp_dir().join(format!("efference-burn-test-{n:016x}")))
}

// ---- derivation ----

#[test]
fn same_root_derives_same_credentials_across_constructions() {
    let a = Root::new("correct horse battery staple");
    let b = Root::new("correct horse battery staple");
    assert_eq!(a.cookie_key(), b.cookie_key());
    assert_eq!(a.totp_secret(), b.totp_secret());
    assert_eq!(a.bearer(), b.bearer());
}

#[test]
fn env_file_trailing_newline_does_not_change_the_derivation() {
    assert_eq!(Root::new("s3cret").bearer(), Root::new("s3cret\n").bearer());
}

#[test]
fn different_roots_and_different_contexts_diverge() {
    let a = Root::new("root a");
    let b = Root::new("root b");
    assert_ne!(a.bearer(), b.bearer());
    // Cross-purpose: the cookie key and the TOTP secret share no prefix.
    assert_ne!(a.cookie_key()[..20], a.totp_secret()[..]);
}

#[test]
fn bearer_matches_accepts_the_derived_token_and_nothing_else() {
    let root = Root::new("root");
    assert!(root.bearer_matches(&root.bearer()));
    assert!(!root.bearer_matches(""));
    assert!(!root.bearer_matches("not-the-bearer"));
    let mut off_by_one = root.bearer();
    off_by_one.pop();
    assert!(!root.bearer_matches(&off_by_one));
}

/// The VAPID keypair is a derivation like the others: same root in, same key out,
/// every call and every construction. Push subscriptions are created against this
/// key, so a key that moved between two daemons on one machine would strand them.
#[test]
fn vapid_key_is_stable_across_constructions() {
    let a = Root::new("correct horse battery staple");
    let b = Root::new("correct horse battery staple");
    assert_eq!(a.vapid_key().to_bytes(), b.vapid_key().to_bytes());
    assert_eq!(a.vapid_key().to_bytes(), a.vapid_key().to_bytes());
    assert_ne!(
        a.vapid_key().to_bytes(),
        Root::new("another root").vapid_key().to_bytes()
    );
}

/// One known root's public key, pinned. This is the value browsers subscribed
/// against, so a change to the context string or the reject-and-retry loop moves
/// it and strands every subscription on the machine — it must break a test first.
#[test]
fn a_known_root_derives_a_fixed_public_key() {
    use p256::elliptic_curve::sec1::ToSec1Point;
    let key = Root::new("correct horse battery staple").vapid_key();
    assert_eq!(
        URL_SAFE_NO_PAD.encode(key.public_key().to_uncompressed_point()),
        "BNZc0T9OXhsNgF0IrSEkFk817b_bd8xg-oEsUGiyC00dAkhVEDijHxfwQo88W2cOiftt-kWMGcWD3XU3bBZalkI"
    );
}

// ---- cookie ----

#[test]
fn cookie_roundtrips_within_ttl() {
    let key = Root::new("root").cookie_key();
    let issued = at(1_700_000_000);
    let cookie = mint_cookie(&key, issued);
    assert!(cookie_is_valid(&key, &cookie, issued));
    assert!(cookie_is_valid(&key, &cookie, issued + COOKIE_TTL));
}

#[test]
fn cookie_expires_after_ttl_and_rejects_future_issued_at() {
    let key = Root::new("root").cookie_key();
    let issued = at(1_700_000_000);
    let cookie = mint_cookie(&key, issued);
    assert!(!cookie_is_valid(
        &key,
        &cookie,
        issued + COOKIE_TTL + Duration::from_secs(1)
    ));
    // A cookie "issued" after now never verifies — there is no legitimate path to one.
    assert!(!cookie_is_valid(
        &key,
        &cookie,
        issued - Duration::from_secs(1)
    ));
}

#[test]
fn tampered_and_malformed_cookies_are_rejected() {
    let key = Root::new("root").cookie_key();
    let cookie = mint_cookie(&key, at(1_700_000_000));
    let mut bytes = URL_SAFE_NO_PAD.decode(&cookie).unwrap();
    bytes[0] ^= 1; // move the signed issued-at
    assert!(!cookie_is_valid(
        &key,
        &URL_SAFE_NO_PAD.encode(&bytes),
        at(1_700_000_001)
    ));
    assert!(!cookie_is_valid(&key, "", at(1_700_000_001)));
    assert!(!cookie_is_valid(&key, "not base64url!!", at(1_700_000_001)));
    // Authentic under a different key ≠ authentic under this one.
    let other = Root::new("other root").cookie_key();
    assert!(!cookie_is_valid(&other, &cookie, at(1_700_000_001)));
}

// ---- TOTP ----

/// RFC 6238 Appendix B vectors (SHA-1 secret `12345678901234567890`), truncated from
/// the spec's eight digits to our six — i.e. the same codes mod 10⁶.
#[test]
fn rfc6238_sha1_vectors() {
    let secret = b"12345678901234567890";
    for (time, code) in [
        (59_u64, 287082_u32),
        (1_111_111_109, 81804),
        (1_111_111_111, 50471),
        (1_234_567_890, 5924),
        (2_000_000_000, 279037),
        (20_000_000_000, 353130),
    ] {
        assert_eq!(totp(secret, time / TOTP_STEP_SECS), code, "at time {time}");
    }
}

#[test]
fn format_code_keeps_leading_zeros() {
    assert_eq!(format_code(5924), "005924");
    assert_eq!(format_code(287082), "287082");
}

#[test]
fn matching_timestep_accepts_the_skew_window_and_nothing_wider() {
    let secret = Root::new("root").totp_secret();
    let now = at(1_700_000_015); // mid-step
    let t = timestep(now);
    for step in [t - 1, t, t + 1] {
        let code = format_code(totp(&secret, step));
        assert_eq!(matching_timestep(&secret, &code, now), Some(step));
    }
    for step in [t - 2, t + 2] {
        let code = format_code(totp(&secret, step));
        assert_eq!(matching_timestep(&secret, &code, now), None);
    }
}

#[test]
fn matching_timestep_rejects_malformed_codes() {
    let secret = Root::new("root").totp_secret();
    let now = at(1_700_000_015);
    let valid = format_code(totp(&secret, timestep(now)));
    assert!(matching_timestep(&secret, &valid[..5], now).is_none()); // five digits
    assert!(matching_timestep(&secret, "12345a", now).is_none());
    assert!(matching_timestep(&secret, "", now).is_none());
}

#[test]
fn otpauth_uri_carries_the_base32_secret_and_stock_parameters() {
    let secret = Root::new("root").totp_secret();
    let uri = otpauth_uri(&secret, "example.ts.net");
    assert!(uri.starts_with("otpauth://totp/efference:example.ts.net?"));
    assert!(uri.contains(&format!(
        "secret={}",
        data_encoding::BASE32_NOPAD.encode(&secret)
    )));
    assert!(uri.contains("algorithm=SHA1"));
    assert!(uri.contains("digits=6"));
    assert!(uri.contains("period=30"));
}

// ---- burn ledger ----

#[test]
fn a_step_burns_once() {
    let ledger = scratch_ledger();
    assert!(ledger.burn(1000).unwrap());
    assert!(!ledger.burn(1000).unwrap());
    assert!(ledger.burn(1001).unwrap());
}

#[test]
fn burns_survive_reopening_the_ledger() {
    let ledger = scratch_ledger();
    let path = ledger.path.clone();
    assert!(ledger.burn(2000).unwrap());
    drop(ledger);
    assert!(!BurnLedger::new(path).burn(2000).unwrap());
}

#[test]
fn stale_burns_are_pruned_but_the_skew_window_is_kept() {
    let ledger = scratch_ledger();
    assert!(ledger.burn(1000).unwrap());
    assert!(ledger.burn(1000 + RETAIN_STEPS + 1).unwrap());
    // 1000 fell out of retention: burnable again (harmless — it can no longer verify).
    assert!(ledger.burn(1000).unwrap());
    // A step within retention of the newest burn is still remembered.
    assert!(ledger.burn(1000 + RETAIN_STEPS).unwrap());
    assert!(!ledger.burn(1000 + RETAIN_STEPS).unwrap());
}

// ---- lockout ----

#[test]
fn lockout_engages_after_the_free_budget_and_doubles() {
    let mut lockout = Lockout::default();
    let t0 = Instant::now();
    for _ in 0..LOCKOUT_FREE_FAILURES - 1 {
        lockout.failure(t0);
    }
    assert!(lockout.locked_for(t0).is_none());
    lockout.failure(t0);
    assert_eq!(lockout.locked_for(t0), Some(LOCKOUT_BASE));
    lockout.failure(t0);
    assert_eq!(lockout.locked_for(t0), Some(LOCKOUT_BASE * 2));
    // The cap holds no matter how many failures accumulate.
    for _ in 0..40 {
        lockout.failure(t0);
    }
    assert_eq!(lockout.locked_for(t0), Some(LOCKOUT_MAX));
}

#[test]
fn lockout_expires_with_time_and_clears_on_success() {
    let mut lockout = Lockout::default();
    let t0 = Instant::now();
    for _ in 0..LOCKOUT_FREE_FAILURES {
        lockout.failure(t0);
    }
    assert!(lockout.locked_for(t0 + LOCKOUT_BASE).is_none());
    lockout.success();
    assert!(lockout.locked_for(t0).is_none());
    // The free budget is fresh after a success.
    lockout.failure(t0);
    assert!(lockout.locked_for(t0).is_none());
}
