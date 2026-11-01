use super::fixtures::*;
use super::*;

fn test_key() -> PublicKey {
    PublicKey::from_base64(TEST_KEY).unwrap()
}

fn newer() -> Release {
    Release {
        version: "9.0.0".into(),
        tag: "v9.0.0".into(),
        url: "https://efference.net/releases/v9.0.0".into(),
    }
}

/// The compiled-in key decodes, so the published check can be built at all.
#[test]
fn the_release_key_is_a_minisign_key() {
    PublicKey::from_base64(RELEASE_PUBLIC_KEY).expect("decodes");
    let check = UpdateCheck::published();
    assert_eq!(check.installed_version(), env!("CARGO_PKG_VERSION"));
}

/// A document signed by the key, for the tag it names, is believed.
#[test]
fn a_signed_document_for_its_own_tag_is_believed() {
    assert_eq!(
        verify(NEWER.as_bytes(), NEWER_SIG, &test_key()),
        Ok(newer())
    );
}

/// The fixtures are not signed by the release key, which is what makes a check
/// against efference.net refuse anything a test key signed.
#[test]
fn a_test_signature_does_not_verify_against_the_release_key() {
    let release = PublicKey::from_base64(RELEASE_PUBLIC_KEY).unwrap();
    let err = verify(NEWER.as_bytes(), NEWER_SIG, &release).unwrap_err();
    assert!(err.contains("does not verify"), "{err}");
}

/// A valid signature whose trusted comment names another release or another file
/// is refused: the comment is what stops a signature from being presented for a
/// document it was not made for.
#[test]
fn a_signature_made_for_another_tag_or_file_is_refused() {
    for signature in [NEWER_SIG_OLD_TAG, NEWER_SIG_OTHER_FILE] {
        let err = verify(NEWER.as_bytes(), signature, &test_key()).unwrap_err();
        assert!(
            err.contains("was made for") && err.contains("efference v9.0.0 latest.json"),
            "{err}"
        );
    }
}

/// Another key's signature, a document changed after signing, and a trusted
/// comment changed after signing all fail to verify.
#[test]
fn a_signature_that_does_not_check_is_refused() {
    let key = test_key();
    let err = verify(NEWER.as_bytes(), NEWER_SIG_OTHER_KEY, &key).unwrap_err();
    assert!(err.contains("does not verify"), "{err}");

    let tampered = NEWER.replace("9.0.0", "9.0.1");
    let err = verify(tampered.as_bytes(), NEWER_SIG, &key).unwrap_err();
    assert!(err.contains("does not verify"), "{err}");

    let relabelled = NEWER_SIG_OLD_TAG.replace(
        "trusted comment: efference v0.0.1 latest.json",
        "trusted comment: efference v9.0.0 latest.json",
    );
    let err = verify(NEWER.as_bytes(), &relabelled, &key).unwrap_err();
    assert!(err.contains("does not verify"), "{err}");
}

/// Text that is not a signature at all is a failed check, not a panic.
#[test]
fn a_malformed_signature_is_refused() {
    let err = verify(NEWER.as_bytes(), "not a signature", &test_key()).unwrap_err();
    assert!(err.contains("not a minisign signature"), "{err}");
}

/// Versions compare as semver: a stable release is newer than its own prerelease,
/// and neither the same version nor an older one is newer.
#[test]
fn newer_is_semver_order() {
    let release = |version: &str| Release {
        version: version.into(),
        tag: format!("v{version}"),
        url: String::new(),
    };
    assert_eq!(release("0.2.0").is_newer_than("0.1.0"), Ok(true));
    assert_eq!(release("0.2.0").is_newer_than("0.2.0-rc.1"), Ok(true));
    assert_eq!(release("0.10.0").is_newer_than("0.9.0"), Ok(true));
    assert_eq!(release("0.2.0").is_newer_than("0.2.0"), Ok(false));
    assert_eq!(release("0.1.0").is_newer_than("0.2.0"), Ok(false));
    assert!(release("latest").is_newer_than("0.1.0").is_err());
}

/// The check fetches both files and believes what they say together.
#[tokio::test]
async fn the_check_fetches_the_document_and_its_signature() {
    let check = check(&[
        ("/releases/latest.json", NEWER),
        ("/releases/latest.json.minisig", NEWER_SIG),
    ]);
    assert_eq!(check.newest().await, Ok(newer()));
    assert_eq!(
        check.clone().installed("0.1.0").newer().await,
        Ok(Some(newer()))
    );
    assert_eq!(check.installed("9.0.0").newer().await, Ok(None));
}

/// A release that is not newer than the installed version is no update.
#[tokio::test]
async fn an_older_release_is_no_update() {
    let check = check(&[
        ("/releases/latest.json", OLDER),
        ("/releases/latest.json.minisig", OLDER_SIG),
    ]);
    assert_eq!(check.installed("0.1.0").newer().await, Ok(None));
}

/// A document served without its signature is a failed check, never a version.
#[tokio::test]
async fn a_missing_signature_fails_the_check() {
    let check = check(&[("/releases/latest.json", NEWER)]);
    let err = check.newer().await.unwrap_err();
    assert!(err.contains("latest.json.minisig answered 404"), "{err}");

    let err = self::check(&[]).newest().await.unwrap_err();
    assert!(err.contains("latest.json answered 404"), "{err}");
}

/// A signature the key does not verify fails the check the same way.
#[tokio::test]
async fn a_bad_signature_fails_the_check() {
    let check = check(&[
        ("/releases/latest.json", NEWER),
        ("/releases/latest.json.minisig", NEWER_SIG_OTHER_KEY),
    ]);
    assert!(check.newer().await.is_err());
}
