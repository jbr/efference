//! RFC 8292 VAPID: the application server's self-identification (docs/web-push.md).
//!
//! A push request carries `Authorization: vapid t=<jwt>, k=<key>`, where the JWT is
//! signed ES256 by the machine's derived VAPID keypair (`auth.rs`) and `k` is that
//! keypair's public half in uncompressed X9.62 form. The push service uses `k` to
//! verify `t`, which binds a subscription's messages to one stable sender identity.
//!
//! The JWT is assembled here rather than through a JWT crate: it is two fixed JSON
//! objects and one signature, and the encodings are pinned in `test.rs` — a
//! general-purpose encoder would put a layer between this code and what the RFC
//! says goes on the wire.
//!
//! No `sub` claim is sent. RFC 8292 §2.1 makes it optional, and the only values it
//! would carry are a contact `mailto:`/`https:` for the operator — a personal
//! identifier handed to every push service the subscriptions live on.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p256::SecretKey;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToSec1Point;

/// How long a minted token is valid. RFC 8292 §2 caps `exp` at 24 hours from the
/// request; half that leaves the token comfortably inside the cap under any clock
/// skew a push service might apply, and the token is minted per request anyway.
pub(super) const TOKEN_TTL_SECS: u64 = 12 * 60 * 60;

/// The `{"typ":"JWT","alg":"ES256"}` header, pre-encoded: it is constant, and the
/// exact bytes are what the signature covers.
const HEADER_B64: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9";

/// The origin a push endpoint belongs to — the `aud` claim (RFC 8292 §2), which
/// binds a token to one push service rather than one endpoint URL.
///
/// Scheme and authority as written, with no path: a push endpoint is always an
/// absolute `https:` URL with an explicit host, so no relative or default-port
/// normalization has anything to do here. `None` when the string is not one.
pub(super) fn audience(endpoint: &str) -> Option<String> {
    let (scheme, rest) = endpoint.split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

/// The ES256 JWT for one audience, expiring at `exp` (seconds since the Unix
/// epoch). `exp` is a parameter rather than read from the clock so the encoding
/// is a pure function of its inputs and can be pinned.
pub(super) fn token(key: &SecretKey, audience: &str, exp: u64) -> String {
    // Claim order is fixed here rather than left to a map: the signed input is
    // bytes, so any reordering is a different token. `aud` is serialized through
    // serde so a host with a character needing escaping cannot break the JSON.
    let aud = serde_json::Value::String(audience.to_string());
    let claims = format!(r#"{{"aud":{aud},"exp":{exp}}}"#);
    let signing_input = format!("{HEADER_B64}.{}", B64.encode(claims));
    let signing_key = SigningKey::from(key);
    let signature: Signature = signing_key.sign(signing_input.as_bytes());
    // JWS ES256 is the fixed-width r‖s pair, not the ASN.1 DER form.
    format!("{signing_input}.{}", B64.encode(signature.to_bytes()))
}

/// The `k` parameter: the signing key's public half, uncompressed X9.62,
/// base64url. Also what `GET /v1/push` hands the browser as its
/// `applicationServerKey` — the same key in the same encoding, so a subscription
/// is created for exactly the sender that will use it.
pub(super) fn public_key_param(key: &SecretKey) -> String {
    B64.encode(key.public_key().to_uncompressed_point())
}

/// The complete `Authorization` header value for one request.
pub(super) fn authorization(key: &SecretKey, audience: &str, exp: u64) -> String {
    format!(
        "vapid t={}, k={}",
        token(key, audience, exp),
        public_key_param(key)
    )
}

#[cfg(test)]
mod test;
