//! The VAPID JWT's encoding and signature, pinned.

use super::*;
use p256::ecdsa::VerifyingKey;
use p256::ecdsa::signature::Verifier;

/// The RFC 8291 example's sender scalar, reused here purely as a fixed key: this
/// test is about the JWT, and any known scalar serves.
const KEY: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";

fn key() -> SecretKey {
    SecretKey::from_slice(&B64.decode(KEY).unwrap()).unwrap()
}

/// The pre-encoded header is the base64url of the JSON RFC 8292 §2.4 shows.
#[test]
fn header_encodes_the_documented_json() {
    let decoded = B64.decode(HEADER_B64).expect("valid base64url");
    assert_eq!(decoded, br#"{"typ":"JWT","alg":"ES256"}"#);
}

/// `aud` is the push endpoint's origin: scheme and authority, path dropped, port
/// kept where one is written.
#[test]
fn audience_is_the_endpoint_origin() {
    assert_eq!(
        audience("https://push.example.net/p/JzLQ3raZJfFBR0aqvOMsLrt54w4rJUsV").as_deref(),
        Some("https://push.example.net")
    );
    assert_eq!(
        audience("https://fcm.googleapis.com/fcm/send/abc?x=1").as_deref(),
        Some("https://fcm.googleapis.com")
    );
    assert_eq!(
        audience("https://push.example.net:8443/p/x").as_deref(),
        Some("https://push.example.net:8443")
    );
    assert_eq!(
        audience("https://push.example.net").as_deref(),
        Some("https://push.example.net")
    );
    assert_eq!(audience("not a url"), None);
    assert_eq!(audience("https:///p/x"), None);
}

/// The signed halves, for fixed inputs. Header and payload are pinned as exact
/// base64url; the signature is verified rather than pinned because it is over
/// those bytes and a pin would say nothing more.
#[test]
fn token_encoding_and_signature() {
    // The claim set from RFC 8292 §2.4's example, minus `sub`, which this
    // harness never sends.
    let jwt = token(&key(), "https://push.example.net", 1453523768);
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3, "header.payload.signature");
    assert_eq!(parts[0], HEADER_B64);
    assert_eq!(
        String::from_utf8(B64.decode(parts[1]).unwrap()).unwrap(),
        r#"{"aud":"https://push.example.net","exp":1453523768}"#,
        "claims, in the order they are signed in"
    );
    assert_eq!(
        parts[1], "eyJhdWQiOiJodHRwczovL3B1c2guZXhhbXBsZS5uZXQiLCJleHAiOjE0NTM1MjM3Njh9",
        "the payload's exact bytes"
    );

    let signature =
        Signature::from_slice(&B64.decode(parts[2]).unwrap()).expect("64 octets of r‖s, not DER");
    let verifying = VerifyingKey::from(key().public_key());
    verifying
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .expect("the derived public key verifies its own token");

    // A token over different claims does not verify against the same signature —
    // the signed input really is the two encoded halves.
    let other = token(&key(), "https://push.example.com", 1453523768);
    assert!(
        verifying
            .verify(
                format!("{}.{}", parts[0], other.split('.').nth(1).unwrap()).as_bytes(),
                &signature
            )
            .is_err()
    );
}

/// The whole header value, with `k` carrying the uncompressed public point that
/// verifies `t`.
#[test]
fn authorization_carries_token_and_key() {
    let key = key();
    let header = authorization(&key, "https://push.example.net", 1453523768);
    let (t, k) = header
        .strip_prefix("vapid t=")
        .and_then(|rest| rest.split_once(", k="))
        .expect("vapid t=…, k=…");
    assert_eq!(t, token(&key, "https://push.example.net", 1453523768));

    let point = B64.decode(k).expect("base64url");
    assert_eq!(point.len(), 65, "uncompressed X9.62");
    assert_eq!(point[0], 0x04, "the uncompressed point tag");
    assert_eq!(
        p256::PublicKey::from_sec1_bytes(&point).unwrap(),
        key.public_key(),
        "k is the signing key's public half"
    );
}
