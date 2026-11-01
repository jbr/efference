use super::*;
use crate::platform::devices::DeviceId;

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

/// Rotation writes a fresh root over the old, and the next load reads it: two mints
/// differ, since the root is random, not derived from anything.
#[test]
fn rotation_writes_a_different_root_that_the_next_load_reads() {
    let secrets = Secrets::in_memory();
    let before = Root::load(&secrets).unwrap();
    let first = secrets.get(ROOT_ITEM).unwrap().expect("minted");
    Root::rotate(&secrets).unwrap();
    let second = secrets.get(ROOT_ITEM).unwrap().expect("rotated");
    assert_eq!(second.len(), 43, "{second}");
    assert_ne!(first, second);
    let after = Root::load(&secrets).unwrap();
    let id = DeviceId::random();
    let token = before.device_tokens().token(&id);
    assert_eq!(after.device_tokens().parse(&token), None);
    assert_eq!(
        after.device_tokens().token(&id),
        Root::new(&second).device_tokens().token(&id)
    );
    assert_ne!(before.vapid_key().to_bytes(), after.vapid_key().to_bytes());
}

/// An unreadable store names the way out and what it costs.
#[test]
fn an_unreadable_root_points_at_rotation() {
    let error = RootError::Unreadable("could not read secret `root`: keychain locked".into());
    let text = error.to_string();
    assert!(text.starts_with("could not read secret `root`: keychain locked; "));
    assert!(text.contains("`efference auth rotate`"));
    assert!(text.contains("signs out every enrolled device"));
}

/// An empty root would derive credentials anyone could compute, so it is refused
/// like an unreadable one rather than used.
#[test]
fn an_empty_root_is_refused() {
    let secrets = Secrets::in_memory();
    secrets.set(ROOT_ITEM, " \n").unwrap();
    let Err(error) = Root::load(&secrets) else {
        panic!("an empty root loaded");
    };
    assert!(matches!(error, RootError::Unreadable(_)));
    assert!(
        error
            .to_string()
            .starts_with("the `root` item in process memory is empty; `efference auth rotate`"),
        "{error}"
    );
}

/// Through the `[secrets]` commands a bind never mints: an absent root fails the load,
/// naming the verb that makes one, and writes nothing. `efference auth rotate` writes
/// one with `set`, which every load after reads.
#[test]
fn under_the_commands_an_absent_root_fails_the_bind_until_rotate_writes_one() {
    let store = crate::platform::secrets::testing::ScriptStore::new("root", "absent_status = 3");
    let secrets = store.secrets();
    let Err(error) = Root::load(&secrets) else {
        panic!("a root was minted through the commands");
    };
    assert!(matches!(error, RootError::Absent { .. }));
    assert_eq!(
        error.to_string(),
        format!(
            "no web root is stored in the `[secrets]` store; `efference auth rotate` creates \
             one, which {ROTATION_CONSEQUENCES}"
        )
    );
    assert_eq!(store.item(ROOT_ITEM), None);

    Root::rotate(&secrets).unwrap();
    let stored = store.item(ROOT_ITEM).expect("rotate wrote the root");
    assert_eq!(stored.len(), 43, "{stored}");
    let id = DeviceId::random();
    let loaded = Root::load(&secrets).unwrap();
    assert_eq!(
        loaded.device_tokens().token(&id),
        Root::new(&stored).device_tokens().token(&id)
    );
    assert_eq!(
        Root::load(&store.secrets())
            .unwrap()
            .device_tokens()
            .token(&id),
        loaded.device_tokens().token(&id)
    );
}

/// A failure of `get` other than the absent status is unreadable, not absent, and is
/// not minted over either.
#[test]
fn under_the_commands_a_failed_read_is_not_absence() {
    let store =
        crate::platform::secrets::testing::ScriptStore::new("root-sealed", "absent_status = 3");
    store.script("sealed.sh", "echo 'vault is sealed' >&2; exit 2");
    store.configure(&format!(
        "get = {}\nset = {}\nabsent_status = 3",
        store.argv("sealed.sh"),
        store.argv("set.sh")
    ));
    let Err(error) = Root::load(&store.secrets()) else {
        panic!("a sealed store loaded a root");
    };
    assert!(matches!(error, RootError::Unreadable(_)));
    assert!(error.to_string().contains("vault is sealed"), "{error}");
    assert_eq!(store.item(ROOT_ITEM), None);
}

// ---- derivation ----

#[test]
fn same_root_derives_same_credentials_across_constructions() {
    let a = Root::new("correct horse battery staple");
    let b = Root::new("correct horse battery staple");
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
    // Cross-purpose: the token key and the push-signing key are different bytes.
    assert_ne!(a.device_tokens().0[..], a.vapid_key().to_bytes()[..]);
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
