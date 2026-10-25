//! The device table (docs/web-bind.md, docs/state-store.md): what a row holds, how a
//! request finds it, and how often finding it writes.

use super::*;
use std::time::Duration;

/// A throwaway store under the OS temp dir (the scratch idiom the other state tests
/// use; no tempfile dependency).
fn scratch() -> Store {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let n = RandomState::new().build_hasher().finish();
    Store::new(std::env::temp_dir().join(format!("efference-devices-test-{n:016x}/state.db")))
}

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

fn last_seen(store: &Store, id: &DeviceId) -> Option<i64> {
    let devices = store.list().unwrap();
    let device = devices
        .iter()
        .find(|device| device.id == *id)
        .expect("listed");
    device.last_seen_at
}

#[test]
fn an_enrolled_device_lists_with_what_it_was_given() {
    let store = scratch();
    let browser = store
        .insert(Kind::Browser, "Safari on macOS", Joined::Code, at(1_000))
        .unwrap();
    let script = store
        .insert(Kind::Script, "ci", Joined::Add, at(2_000))
        .unwrap();
    assert_ne!(browser.id, script.id, "every enrollment draws its own id");
    assert_eq!(
        store.list().unwrap(),
        [
            Device {
                id: browser.id,
                kind: Kind::Browser,
                label: "Safari on macOS".into(),
                joined: Joined::Code,
                created_at: 1_000,
                last_seen_at: None,
            },
            Device {
                id: script.id,
                kind: Kind::Script,
                label: "ci".into(),
                joined: Joined::Add,
                created_at: 2_000,
                last_seen_at: None,
            },
        ],
        "oldest first, as inserted"
    );
}

/// The row's existence is the answer, and a revoked row is gone for good.
#[test]
fn seen_answers_whether_the_row_exists() {
    let store = scratch();
    assert!(!store.seen(&DeviceId::random(), at(1_000)).unwrap());
    let device = store
        .insert(Kind::Script, "ci", Joined::Add, at(1_000))
        .unwrap();
    assert!(store.seen(&device.id, at(1_001)).unwrap());
    assert!(store.remove(&device.id).unwrap());
    assert!(!store.seen(&device.id, at(1_002)).unwrap());
    assert!(
        !store.remove(&device.id).unwrap(),
        "a second revoke finds nothing"
    );
}

/// `last_seen_at` is written by the first request and then at most once an hour, so
/// a burst of requests is reads.
#[test]
fn last_seen_is_written_once_an_hour_at_most() {
    let store = scratch();
    let device = store
        .insert(Kind::Browser, "", Joined::Code, at(10_000))
        .unwrap();
    assert_eq!(last_seen(&store, &device.id), None);

    assert!(store.seen(&device.id, at(10_005)).unwrap());
    assert_eq!(last_seen(&store, &device.id), Some(10_005));
    for later in [10_006, 10_005 + 1_800, 10_005 + SEEN_INTERVAL_SECS as u64] {
        assert!(store.seen(&device.id, at(later)).unwrap());
        assert_eq!(
            last_seen(&store, &device.id),
            Some(10_005),
            "seen at {later}, inside the hour"
        );
    }
    let past = 10_005 + SEEN_INTERVAL_SECS as u64 + 1;
    assert!(store.seen(&device.id, at(past)).unwrap());
    assert_eq!(last_seen(&store, &device.id), Some(past as i64));
}

/// What rotation does beside replacing the root: every row goes.
#[test]
fn clear_empties_the_table() {
    let store = scratch();
    let kept = store
        .insert(Kind::Browser, "phone", Joined::Code, at(1))
        .unwrap();
    store
        .insert(Kind::Script, "ci", Joined::Add, at(2))
        .unwrap();
    assert_eq!(store.clear().unwrap(), 2);
    assert!(store.list().unwrap().is_empty());
    assert!(!store.seen(&kept.id, at(3)).unwrap());
    assert_eq!(store.clear().unwrap(), 0);
}

/// A table that cannot be read is an error to the gate's lookup and to the listing,
/// never an empty answer: the one would admit nothing it could not check, and the
/// other would hide a row whose token still works.
#[test]
fn a_file_that_is_not_a_database_is_an_error() {
    let store = scratch();
    std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
    std::fs::write(&store.path, "{ not a database").unwrap();
    assert!(store.seen(&DeviceId::random(), at(1)).is_err());
    assert!(store.list().is_err());
    assert!(
        store
            .insert(Kind::Script, "ci", Joined::Add, at(1))
            .is_err()
    );
}

/// A row this build does not name fails the listing rather than dropping out of it.
#[test]
fn an_unrecognized_row_fails_the_listing() {
    let store = scratch();
    let device = store
        .insert(Kind::Browser, "phone", Joined::Code, at(1))
        .unwrap();
    store
        .connect()
        .unwrap()
        .execute(
            "UPDATE devices SET joined = 'carrier pigeon' WHERE id = ?1",
            [&device.id],
        )
        .unwrap();
    let error = store.list().unwrap_err().to_string();
    assert!(error.contains("carrier pigeon"), "{error}");
    assert!(
        store.seen(&device.id, at(2)).unwrap(),
        "the row still authenticates"
    );
}

/// Requester-supplied text reaches the operator's terminal: nothing that steers it
/// survives, and nothing past the cap.
#[test]
fn labels_are_cleaned_on_the_way_in() {
    assert_eq!(clean_label("Safari on macOS", "browser"), "Safari on macOS");
    assert_eq!(
        clean_label("\u{1b}[2J\u{1b}]0;owned\u{7}phone\r\n", "browser"),
        "[2J ]0;owned phone"
    );
    assert_eq!(
        clean_label("evil\u{202E}txt.exe\u{2066}x", "browser"),
        "evil txt.exe x"
    );
    assert_eq!(clean_label("  two   words\t", "browser"), "two words");
    assert_eq!(clean_label("", "browser"), "browser");
    assert_eq!(clean_label("\u{1b}\u{7}\n", "script"), "script");
    let long = "é".repeat(LABEL_MAX_CHARS + 10);
    assert_eq!(
        clean_label(&long, "browser").chars().count(),
        LABEL_MAX_CHARS
    );
    // The cap counts characters, and a cut that lands after a space keeps none.
    let spaced = format!("{} tail", "a".repeat(LABEL_MAX_CHARS - 1));
    assert_eq!(
        clean_label(&spaced, "browser"),
        "a".repeat(LABEL_MAX_CHARS - 1)
    );

    let store = scratch();
    let device = store
        .insert(Kind::Browser, "\u{1b}[31mred", Joined::Code, at(1))
        .unwrap();
    assert_eq!(device.label, "[31mred");
    assert_eq!(store.list().unwrap()[0].label, "[31mred");
    let unnamed = store.insert(Kind::Script, " ", Joined::Add, at(2)).unwrap();
    assert_eq!(unnamed.label, "script", "an empty label is the kind's name");
}

#[test]
fn an_id_is_spelled_in_lowercase_hex() {
    let id = DeviceId::from_bytes([0xab; DeviceId::LEN]);
    assert_eq!(id.hex(), "ab".repeat(DeviceId::LEN));
    assert_eq!(id.short(), "abababab");
    assert_eq!(DeviceId::parse_hex(&id.hex()), Some(id));
    assert_eq!(DeviceId::parse_hex(&id.hex().to_uppercase()), Some(id));
    assert_eq!(DeviceId::parse_hex(&id.hex()[..30]), None, "too short");
    assert_eq!(DeviceId::parse_hex(&format!("{}00", id.hex())), None);
    assert_eq!(DeviceId::parse_hex(&"zz".repeat(DeviceId::LEN)), None);
    assert_eq!(
        serde_json::to_value(id).unwrap(),
        serde_json::json!(id.hex())
    );
    assert_eq!(
        serde_json::from_value::<DeviceId>(serde_json::json!(id.hex())).unwrap(),
        id
    );
}

/// The wire spelling of the enums is the table's: `as_str` and serde agree.
#[test]
fn kinds_and_joins_have_one_spelling() {
    for kind in Kind::ALL {
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
    }
    for joined in Joined::ALL {
        assert_eq!(serde_json::to_value(joined).unwrap(), joined.as_str());
    }
}
