use super::*;
use crate::devices::DeviceId;
use std::time::UNIX_EPOCH;

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

/// A throwaway ledger database under the OS temp dir (same create-or-retry entropy idiom
/// as the scratch dir; no tempfile dependency).
fn scratch_ledger() -> BurnLedger {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let n = RandomState::new().build_hasher().finish();
    BurnLedger::new(std::env::temp_dir().join(format!("efference-burn-test-{n:016x}")))
}

// ---- the stored root ----

/// The first load mints; every load after reads the same item back.
#[test]
fn the_root_is_minted_once_and_read_after() {
    let secrets = Secrets::in_memory();
    assert_eq!(secrets.get(ROOT_ITEM).unwrap(), None);
    let first = Root::load(&secrets).unwrap();
    let stored = secrets.get(ROOT_ITEM).unwrap().expect("minted");
    // 32 random bytes, base64url without padding.
    assert_eq!(stored.len(), 43, "{stored}");
    let second = Root::load(&secrets).unwrap();
    let id = DeviceId::random();
    let token = first.device_tokens().token(&id);
    assert_eq!(token, second.device_tokens().token(&id));
    assert_eq!(token, Root::new(&stored).device_tokens().token(&id));
}

/// Two mints differ: the root is random, not derived from anything.
#[test]
fn rotation_deletes_the_item_and_the_next_load_mints_a_different_root() {
    let secrets = Secrets::in_memory();
    let before = Root::load(&secrets).unwrap();
    Root::rotate(&secrets).unwrap();
    assert_eq!(secrets.get(ROOT_ITEM).unwrap(), None);
    let after = Root::load(&secrets).unwrap();
    let token = before.device_tokens().token(&DeviceId::random());
    assert_eq!(after.device_tokens().parse(&token), None);
    assert_ne!(before.totp_secret(), after.totp_secret());
}

/// An unreadable store names the way out and what it costs.
#[test]
fn an_unreadable_root_points_at_rotation() {
    let error = RootError("could not read secret `root`: keychain locked".into());
    let text = error.to_string();
    assert!(text.starts_with("could not read secret `root`: keychain locked; "));
    assert!(text.contains("`efference auth rotate`"));
    assert!(text.contains("signs out every enrolled device"));
}

// ---- derivation ----

#[test]
fn same_root_derives_same_credentials_across_constructions() {
    let a = Root::new("correct horse battery staple");
    let b = Root::new("correct horse battery staple");
    assert_eq!(a.totp_secret(), b.totp_secret());
    assert_eq!(a.device_tokens().0, b.device_tokens().0);
}

#[test]
fn env_file_trailing_newline_does_not_change_the_derivation() {
    assert_eq!(
        Root::new("s3cret").device_tokens().0,
        Root::new("s3cret\n").device_tokens().0
    );
}

#[test]
fn different_roots_and_different_contexts_diverge() {
    let a = Root::new("root a");
    let b = Root::new("root b");
    assert_ne!(a.device_tokens().0, b.device_tokens().0);
    // Cross-purpose: the token key and the TOTP secret share no prefix.
    assert_ne!(a.device_tokens().0[..20], a.totp_secret()[..]);
}

// ---- device tokens ----

/// A token is the prefix and the base64url of the id and its tag, and it names the
/// id it was minted for.
#[test]
fn a_device_token_names_its_id() {
    let tokens = Root::new("root").device_tokens();
    let id = DeviceId::random();
    let token = tokens.token(&id);
    assert!(token.starts_with("efference_"), "{token}");
    assert_eq!(token.len(), "efference_".len() + 64, "{token}");
    let bytes = URL_SAFE_NO_PAD
        .decode(&token["efference_".len()..])
        .unwrap();
    assert_eq!(&bytes[..DeviceId::LEN], id.as_bytes(), "the id is readable");
    assert_eq!(tokens.parse(&token), Some(id));
    assert_eq!(
        tokens.token(&id),
        token,
        "derived, so minting again is the same token"
    );
    assert_ne!(tokens.token(&DeviceId::random()), token);
}

/// A token whose bytes differ anywhere — the id or the tag — names nothing.
#[test]
fn a_flipped_bit_fails() {
    let tokens = Root::new("root").device_tokens();
    let token = tokens.token(&DeviceId::random());
    let bytes = URL_SAFE_NO_PAD
        .decode(&token["efference_".len()..])
        .unwrap();
    for (index, bit) in [(0, 0), (7, 3), (15, 7), (16, 0), (31, 5), (47, 7)] {
        let mut flipped = bytes.clone();
        flipped[index] ^= 1 << bit;
        let forged = format!("efference_{}", URL_SAFE_NO_PAD.encode(&flipped));
        assert_eq!(tokens.parse(&forged), None, "byte {index} bit {bit}");
    }
}

#[test]
fn a_truncated_or_padded_token_fails() {
    let tokens = Root::new("root").device_tokens();
    let token = tokens.token(&DeviceId::random());
    for cut in [1, 4, 43] {
        assert_eq!(tokens.parse(&token[..token.len() - cut]), None, "cut {cut}");
    }
    assert_eq!(tokens.parse(&format!("{token}A")), None);
    assert_eq!(tokens.parse(&format!("{token}AAAA")), None);
    assert_eq!(tokens.parse(&format!("{token}=")), None);
    assert_eq!(tokens.parse(""), None);
    assert_eq!(tokens.parse("efference_"), None);
}

#[test]
fn a_wrong_prefix_fails() {
    let tokens = Root::new("root").device_tokens();
    let token = tokens.token(&DeviceId::random());
    let encoded = &token["efference_".len()..];
    for prefix in ["", "efference-", "Efference_", "ghp_", "efference__"] {
        assert_eq!(
            tokens.parse(&format!("{prefix}{encoded}")),
            None,
            "{prefix:?}"
        );
    }
}

/// The tag is the root's: a token minted under another root names nothing here, which
/// is what makes rotation revoke every token at once.
#[test]
fn a_token_from_another_root_fails() {
    let id = DeviceId::random();
    let token = Root::new("another root").device_tokens().token(&id);
    assert_eq!(Root::new("root").device_tokens().parse(&token), None);
}

/// One known root's token for one known id, pinned. Browsers and scripts hold tokens
/// minted this way, so a change to the context string, the tag, or the encoding signs
/// out every device on the machine — it must break a test first.
#[test]
fn a_known_root_derives_a_fixed_token() {
    let tokens = Root::new("correct horse battery staple").device_tokens();
    let id = DeviceId::from_bytes(*b"0123456789abcdef");
    assert_eq!(
        tokens.token(&id),
        "efference_MDEyMzQ1Njc4OWFiY2RlZgHWkXQZGmq8VkIZ9cFu5jt8ThARJhs1cm5fdsFRalXU"
    );
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

/// Two daemons, two ledgers, one file: the second burn of a step is a replay.
/// Two handles over one file is the whole cross-daemon case, and the primary key
/// decides it alone.
#[test]
fn a_step_burns_once_across_two_ledgers_on_one_file() {
    let one = scratch_ledger();
    let two = BurnLedger::new(one.path.clone());
    assert!(one.burn(3000).unwrap(), "the first burn wins");
    assert!(!two.burn(3000).unwrap(), "the other daemon sees it burned");
    assert!(two.burn(3001).unwrap(), "a different step is not a replay");
    assert!(!one.burn(3001).unwrap());
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
