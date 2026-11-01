//! RFC 8291 `aes128gcm` payload encryption for Web Push (docs/web-push.md).
//!
//! The subscription hands over a P-256 public key (`p256dh`) and a 16-octet
//! authentication secret (`auth`). A message is encrypted to those two by:
//! ECDH between a freshly generated sender keypair and the subscription's key;
//! HKDF-SHA256 combining that shared secret with the auth secret into the input
//! keying material; then the RFC 8188 derivation of a content encryption key and
//! nonce, and one AES-128-GCM record.
//!
//! Every derivation step is a pure function taking its salt and ephemeral key as
//! parameters rather than sampling them, so the RFC's Appendix A intermediate
//! values are pinned directly (`encrypt/test.rs`).

use aes_gcm::Aes128Gcm;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

/// The record size written into the content-coding header. A push message is a
/// single record (RFC 8291 §4), and the parameter must exceed plaintext +
/// delimiter + tag; 4096 is the RFC's own example value and comfortably above the
/// 4096-octet body a push service is required to carry.
const RECORD_SIZE: u32 = 4096;

/// The padding delimiter that ends the plaintext of the last record (RFC 8188).
/// A receiver must discard a message whose delimiter is anything else.
const LAST_RECORD_DELIMITER: u8 = 0x02;

/// The keys and nonce one message is encrypted under — the RFC 8291 §3.4
/// sequence, kept as a value so the intermediate steps can be asserted rather
/// than inferred from a ciphertext.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Derived {
    /// Input keying material: HKDF over the ECDH secret, salted with the auth secret.
    pub(super) ikm: [u8; 32],
    /// The RFC 8188 pseudorandom key: HKDF-Extract(salt, IKM).
    pub(super) prk: [u8; 32],
    /// The AES-128 content encryption key.
    pub(super) cek: [u8; 16],
    /// The 96-bit GCM nonce. Not XORed with a sequence number: a push message is
    /// one record and record zero's sequence number is zero.
    pub(super) nonce: [u8; 12],
}

/// The RFC 8291 §3.4 derivation, from the values both parties hold.
///
/// `ua_public` and `as_public` are the uncompressed X9.62 point encodings (65
/// octets each, leading `0x04`) — they are hashed into `key_info`, so the
/// encoding is part of the derivation rather than a serialization detail.
pub(super) fn derive(
    ecdh_secret: &[u8],
    auth_secret: &[u8],
    salt: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
) -> Derived {
    // "Use HKDF to combine the ECDH and authentication secrets" — salted with the
    // auth secret, so a sender that does not hold it cannot reach the IKM.
    let mut key_info = Vec::with_capacity(14 + ua_public.len() + as_public.len());
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth_secret), ecdh_secret)
        .expand(&key_info, &mut ikm)
        .expect("32 octets is far below the HKDF-SHA256 ceiling");

    // The RFC 8188 half: extract under the record salt, then expand twice.
    let (prk, hkdf) = Hkdf::<Sha256>::extract(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    hkdf.expand(b"Content-Encoding: aes128gcm\0", &mut cek)
        .expect("16 octets is far below the HKDF-SHA256 ceiling");
    let mut nonce = [0u8; 12];
    hkdf.expand(b"Content-Encoding: nonce\0", &mut nonce)
        .expect("12 octets is far below the HKDF-SHA256 ceiling");

    Derived {
        ikm,
        prk: prk.into(),
        cek,
        nonce,
    }
}

/// The `aes128gcm` content-coding header (RFC 8188 §2.1): salt, record size,
/// then the key id — which for Web Push is the sender's uncompressed public key
/// (RFC 8291 §4), so the receiver can complete the same ECDH.
fn header(salt: &[u8], as_public: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(salt.len() + 5 + as_public.len());
    out.extend_from_slice(salt);
    out.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    out.push(u8::try_from(as_public.len()).expect("a P-256 point is 65 octets"));
    out.extend_from_slice(as_public);
    out
}

/// Encrypt one push message body, with the sender keypair and salt supplied.
///
/// Every input is a parameter — nothing here samples randomness — which is what
/// lets the RFC's example be reproduced byte for byte. [`encrypt`] is the caller
/// facing form that generates the two.
pub(super) fn encrypt_with(
    ua_public: &PublicKey,
    auth_secret: &[u8],
    as_secret: &SecretKey,
    salt: &[u8; 16],
    plaintext: &[u8],
) -> Result<Vec<u8>, aes_gcm::Error> {
    let as_public = as_secret.public_key().to_uncompressed_point();
    let ua_point = ua_public.to_uncompressed_point();
    let ecdh_secret = as_secret.diffie_hellman(ua_public);
    let keys = derive(
        ecdh_secret.raw_secret_bytes(),
        auth_secret,
        salt,
        &ua_point,
        &as_public,
    );

    // One record: the plaintext with the last-record delimiter appended, and no
    // further padding — a push payload is small and padding to a fixed size would
    // only trade one length signal for another.
    let mut padded = Vec::with_capacity(plaintext.len() + 1);
    padded.extend_from_slice(plaintext);
    padded.push(LAST_RECORD_DELIMITER);

    let cipher = Aes128Gcm::new(&keys.cek.into());
    let ciphertext = cipher.encrypt(
        &keys.nonce.into(),
        Payload {
            msg: &padded,
            aad: b"",
        },
    )?;

    let mut body = header(salt, &as_public);
    body.extend_from_slice(&ciphertext);
    Ok(body)
}

/// Encrypt one push message body to a subscription, sampling the per-message
/// sender keypair and salt. RFC 8291 requires both to be fresh per message: the
/// keypair is what makes the ECDH secret single-use, and the salt is what makes
/// the content encryption key single-use under it.
pub(super) fn encrypt(
    ua_public: &PublicKey,
    auth_secret: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, aes_gcm::Error> {
    let mut salt = [0u8; 16];
    rand::fill(&mut salt);
    encrypt_with(ua_public, auth_secret, &random_secret(), &salt, plaintext)
}

/// A fresh P-256 secret key from the system CSPRNG. Reject-and-retry rather than
/// reduction: a uniform 32-byte string is above the group order with probability
/// ~2⁻³², and resampling keeps the distribution uniform where reducing would not.
pub(super) fn random_secret() -> SecretKey {
    loop {
        let mut bytes = [0u8; 32];
        rand::fill(&mut bytes);
        if let Ok(key) = SecretKey::from_slice(&bytes) {
            return key;
        }
    }
}

#[cfg(test)]
mod test;
