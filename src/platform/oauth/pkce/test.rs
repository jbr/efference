use super::*;

/// The worked example from RFC 7636 Appendix B. This is the one place the
/// `S256` transformation can be checked against an authority rather than
/// against itself.
#[test]
fn the_rfc_7636_test_vector() {
    assert_eq!(
        challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    // The RFC publishes the intermediate digest as a byte array; pinning it
    // separates "we hashed the wrong thing" from "we encoded it wrong".
    assert_eq!(
        Sha256::digest("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".as_bytes())[..],
        [
            19, 211, 30, 150, 26, 26, 216, 236, 47, 22, 177, 12, 76, 152, 46, 8, 118, 168, 120,
            173, 109, 241, 68, 86, 110, 225, 137, 74, 203, 112, 249, 195
        ]
    );
}

#[test]
fn a_generated_pair_is_internally_consistent() {
    let pkce = Pkce::generate();
    assert_eq!(pkce.challenge(), challenge_for(pkce.verifier()));
}

/// RFC 7636 §4.1 fixes the verifier at 43..=128 characters drawn from the
/// unreserved set. Base64url is a subset of it, so nothing needs escaping on
/// the way to the token endpoint.
#[test]
fn the_verifier_meets_the_rfc_length_and_alphabet() {
    let pkce = Pkce::generate();
    assert!(
        (43..=128).contains(&pkce.verifier().len()),
        "verifier was {} characters",
        pkce.verifier().len()
    );
    assert!(
        pkce.verifier()
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)),
        "verifier {} left the unreserved alphabet",
        pkce.verifier()
    );
}

/// The challenge is a SHA-256 digest, so always 32 bytes → 43 base64url
/// characters with no padding. Padding would need percent-encoding in a query
/// string, which is exactly the bug `NO_PAD` avoids.
#[test]
fn the_challenge_is_unpadded_base64url() {
    let challenge = Pkce::generate().challenge;
    assert_eq!(challenge.len(), 43);
    assert!(!challenge.contains('='));
    assert!(!challenge.contains('+'));
    assert!(!challenge.contains('/'));
}

/// Reuse is the failure this guards: a fresh pair per authorization attempt is
/// what keeps an intercepted code from being replayable against a later one.
#[test]
fn each_pair_is_fresh() {
    let (first, second) = (Pkce::generate(), Pkce::generate());
    assert_ne!(first.verifier(), second.verifier());
    assert_ne!(first.challenge(), second.challenge());
    assert_ne!(random_token(), random_token());
}

/// A `Pkce` is likely to be inside a struct that gets logged on an error path.
#[test]
fn debug_does_not_leak_the_verifier() {
    let pkce = Pkce::generate();
    let rendered = format!("{pkce:?}");
    assert!(!rendered.contains(pkce.verifier()));
    assert!(rendered.contains("<redacted>"));
    // The challenge is public — it goes in a URL — so it stays legible.
    assert!(rendered.contains(pkce.challenge()));
}
