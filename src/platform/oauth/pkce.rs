//! PKCE — Proof Key for Code Exchange (RFC 7636), `S256` only.
//!
//! The client generates a high-entropy secret (the *verifier*), sends only its
//! SHA-256 digest (the *challenge*) on the authorization request, and sends
//! the verifier itself on the token request. An attacker who intercepts the
//! authorization code — off the loopback redirect, out of a browser history,
//! from a relay — cannot redeem it without the verifier, which never leaves
//! this process.
//!
//! For a public client (docs/mcp-auth.md) that is not defense in depth, it is
//! the depth: there is no client secret behind it. Hence `S256` only. The
//! `plain` method is in RFC 7636 for clients that cannot compute SHA-256, and
//! offering it would let a downgrade turn the challenge back into the code.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use sha2::{Digest, Sha256};

/// The `code_challenge_method` this client sends, and the only one it accepts
/// in an authorization server's `code_challenge_methods_supported`.
pub const S256: &str = "S256";

/// Bytes of entropy behind a verifier and a state value. 32 bytes encodes to
/// 43 base64url characters, the minimum length RFC 7636 §4.1 allows for a
/// verifier (the maximum is 128).
const ENTROPY_BYTES: usize = 32;

/// A verifier/challenge pair for one authorization request.
///
/// Generate one per authorization attempt and never reuse it: the pair is
/// what binds *this* authorization code to *this* client, and a reused
/// verifier is a verifier an earlier interception can replay.
#[derive(Clone)]
pub struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    /// A fresh pair from the thread RNG.
    pub fn generate() -> Self {
        let verifier = random_token();
        let challenge = challenge_for(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    /// The secret, sent only on the token request.
    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    /// The digest, sent on the authorization request as `code_challenge`.
    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

/// Redacts the verifier. A `Pkce` is likely to be inside something that gets
/// logged on an error path, and the verifier is the one value whose disclosure
/// alongside an intercepted code yields a token.
impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"<redacted>")
            .field("challenge", &self.challenge)
            .finish()
    }
}

/// The `S256` transformation: `base64url(sha256(ascii(verifier)))`, no
/// padding (RFC 7636 §4.2). Separate from [`Pkce::generate`] so the RFC's own
/// test vector can be checked against it.
fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A random URL-safe token: a PKCE verifier, or the `state` value that ties an
/// authorization response back to the request that started it.
///
/// Characters are drawn from base64url, a subset of RFC 7636's unreserved
/// `code_verifier` alphabet, so the value needs no escaping anywhere it goes.
pub fn random_token() -> String {
    let mut bytes = [0u8; ENTROPY_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod test;
