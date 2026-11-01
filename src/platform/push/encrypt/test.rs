//! RFC 8291 §5 and Appendix A, pinned.
//!
//! Every constant below is transcribed from `reference/rfc/rfc8291-message-encryption.txt`
//! with the presentation line-wrapping removed. The example fixes the sender keypair
//! and salt, which is exactly what [`encrypt_with`] takes as parameters, so the
//! intermediate values and the final body are reproducible byte for byte rather than
//! merely round-trippable.

use super::*;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;

const PLAINTEXT: &str = "V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24";
const AS_PUBLIC: &str =
    "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
const UA_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
const AUTH_SECRET: &str = "BTBZMqHH6r4Tts7J_aSIgg";

const ECDH_SECRET: &str = "kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs";
const PRK_KEY: &str = "Snr3JMxaHVDXHWJn5wdC52WjpCtd2EIEGBykDcZW32k";
const KEY_INFO: &str = "V2ViUHVzaDogaW5mbwAEJXGyvs3942BVGq8e0PTNNmwRzr5VX4m8t7GGpTM5FzFo7OLr4BhZe9MEebhuPI-OztV3ylkYfpJGmQ22ggCLDgT-M_SrDepxkU21WCP3O1SUj0EwbZIHMtu5pZpTKGSCIA5Zent7wmC6HCJ5mFgJkuk5cwAvMBKiiujwa7t45ewP";
const IKM: &str = "S4lYMb_L0FxCeq0WhDx813KgSYqU26kOyzWUdsXYyrg";
const PRK: &str = "09_eUZGrsvxChDCGRCdkLiDXrReGOEVeSCdCcPBSJSc";
const CEK_INFO: &str = "Q29udGVudC1FbmNvZGluZzogYWVzMTI4Z2NtAA";
const CEK: &str = "oIhVW04MRdy2XN9CiKLxTg";
const NONCE_INFO: &str = "Q29udGVudC1FbmNvZGluZzogbm9uY2UA";
const NONCE: &str = "4h_95klXJ5E_qnoN";
const HEADER: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
const PADDED_PLAINTEXT: &str = "V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24C";
const CIPHERTEXT: &str =
    "8pfeW0KbunFT06SuDKoJH9Ql87S1QUrdirN6GcG7sFz1y1sqLgVi1VhjVkHsUoEsbI_0LpXMuGvnzQ";
/// The complete request body from §5, header and ciphertext concatenated.
const BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

fn b64(s: &str) -> Vec<u8> {
    B64.decode(s).expect("an RFC constant is valid base64url")
}

/// The RFC's own base64url spellings of the two keypairs decode to the points it
/// names — the check that the `p256` encoding this module relies on (uncompressed
/// X9.62, leading `0x04`) is the one the example is written in.
#[test]
fn rfc_keypairs_round_trip_through_p256() {
    let as_secret = SecretKey::from_slice(&b64(AS_PRIVATE)).expect("the RFC's sender scalar");
    assert_eq!(
        as_secret.public_key().to_uncompressed_point().as_slice(),
        b64(AS_PUBLIC),
        "sender public key"
    );
    let ua_secret = SecretKey::from_slice(&b64(UA_PRIVATE)).expect("the RFC's receiver scalar");
    assert_eq!(
        ua_secret.public_key().to_uncompressed_point().as_slice(),
        b64(UA_PUBLIC),
        "receiver public key"
    );
}

/// The ECDH secret and the `key_info` string that combines it with the auth
/// secret. `key_info` is asserted directly because it is the one derivation input
/// this code *constructs* — the two public keys in a fixed order behind a
/// NUL-terminated label — and getting either wrong yields a message the browser
/// silently fails to decrypt.
#[test]
fn ecdh_secret_and_key_info_match_appendix_a() {
    let as_secret = SecretKey::from_slice(&b64(AS_PRIVATE)).unwrap();
    let ua_public = PublicKey::from_sec1_bytes(&b64(UA_PUBLIC)).unwrap();
    let shared = as_secret.diffie_hellman(&ua_public);
    assert_eq!(shared.raw_secret_bytes().as_slice(), b64(ECDH_SECRET));

    // The receiver reaches the same value from the other side of the exchange.
    let ua_secret = SecretKey::from_slice(&b64(UA_PRIVATE)).unwrap();
    let as_public = PublicKey::from_sec1_bytes(&b64(AS_PUBLIC)).unwrap();
    assert_eq!(
        ua_secret.diffie_hellman(&as_public).raw_secret_bytes(),
        shared.raw_secret_bytes(),
        "both parties derive one secret"
    );

    let mut key_info = Vec::new();
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(&b64(UA_PUBLIC));
    key_info.extend_from_slice(&b64(AS_PUBLIC));
    assert_eq!(key_info, b64(KEY_INFO));

    // HKDF-Extract(salt = auth_secret, IKM = ecdh_secret) — the RFC's PRK_key.
    let (prk_key, _) = Hkdf::<Sha256>::extract(Some(&b64(AUTH_SECRET)), shared.raw_secret_bytes());
    assert_eq!(prk_key.as_slice(), b64(PRK_KEY));
}

/// The four values [`derive`] produces, against Appendix A. The two `info`
/// strings are pinned alongside them: they are literals in the derivation, and a
/// typo in either produces keys that are wrong in a way only a receiver notices.
#[test]
fn derivation_matches_appendix_a() {
    assert_eq!(b"Content-Encoding: aes128gcm\0".as_slice(), b64(CEK_INFO));
    assert_eq!(b"Content-Encoding: nonce\0".as_slice(), b64(NONCE_INFO));

    let derived = derive(
        &b64(ECDH_SECRET),
        &b64(AUTH_SECRET),
        &b64(SALT),
        &b64(UA_PUBLIC),
        &b64(AS_PUBLIC),
    );
    assert_eq!(derived.ikm.as_slice(), b64(IKM), "IKM");
    assert_eq!(derived.prk.as_slice(), b64(PRK), "PRK");
    assert_eq!(derived.cek.as_slice(), b64(CEK), "CEK");
    assert_eq!(derived.nonce.as_slice(), b64(NONCE), "NONCE");
}

/// The content-coding header: salt, `rs` as a big-endian 32-bit record size,
/// the key id length, and the sender's uncompressed public key as the key id.
#[test]
fn header_matches_appendix_a() {
    let bytes = header(&b64(SALT), &b64(AS_PUBLIC));
    assert_eq!(bytes, b64(HEADER));
    assert_eq!(bytes.len(), 86, "the RFC's 86-octet header");
}

/// The whole encryption, end to end: the padded plaintext, the ciphertext, and
/// the concatenated body from §5.
#[test]
fn encryption_reproduces_the_rfc_example() {
    let mut padded = b64(PLAINTEXT);
    padded.push(LAST_RECORD_DELIMITER);
    assert_eq!(padded, b64(PADDED_PLAINTEXT), "padding delimiter appended");

    let as_secret = SecretKey::from_slice(&b64(AS_PRIVATE)).unwrap();
    let ua_public = PublicKey::from_sec1_bytes(&b64(UA_PUBLIC)).unwrap();
    let salt: [u8; 16] = b64(SALT).try_into().unwrap();
    let body = encrypt_with(
        &ua_public,
        &b64(AUTH_SECRET),
        &as_secret,
        &salt,
        &b64(PLAINTEXT),
    )
    .expect("the RFC's inputs encrypt");

    assert_eq!(&body[..86], b64(HEADER), "header half");
    assert_eq!(&body[86..], b64(CIPHERTEXT), "ciphertext half");
    assert_eq!(body, b64(BODY), "the complete §5 body");
}

/// A message encrypted with sampled key and salt decrypts under the
/// subscription's private key — the property the pinned vector cannot show,
/// since it fixes both.
#[test]
fn sampled_key_and_salt_still_decrypt() {
    let ua_secret = random_secret();
    let auth: [u8; 16] = [7; 16];
    let body = encrypt(&ua_secret.public_key(), &auth, b"hello").expect("encrypts");

    // Unpack the header the way a receiver does: salt, rs, keyid length, keyid.
    let (salt, rest) = body.split_at(16);
    let (_rs, rest) = rest.split_at(4);
    let (idlen, rest) = rest.split_at(1);
    let (keyid, ciphertext) = rest.split_at(usize::from(idlen[0]));

    let as_public = PublicKey::from_sec1_bytes(keyid).expect("the keyid is a point");
    let shared = ua_secret.diffie_hellman(&as_public);
    let keys = derive(
        shared.raw_secret_bytes(),
        &auth,
        salt,
        &ua_secret.public_key().to_uncompressed_point(),
        keyid,
    );
    let plaintext = Aes128Gcm::new(&keys.cek.into())
        .decrypt(
            &keys.nonce.into(),
            Payload {
                msg: ciphertext,
                aad: b"",
            },
        )
        .expect("decrypts");
    assert_eq!(plaintext, b"hello\x02");
}
