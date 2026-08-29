//! The subscription store, and the shape it accepts from the browser.

use super::*;

fn sub(endpoint: &str) -> Subscription {
    Subscription {
        endpoint: endpoint.into(),
        p256dh: "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4".into(),
        auth: "BTBZMqHH6r4Tts7J_aSIgg".into(),
    }
}

fn temp_store(name: &str) -> Store {
    let path = std::env::temp_dir().join(format!("eff-push-{name}.json"));
    let _ = std::fs::remove_file(&path);
    Store::new(path)
}

/// The store's whole lifecycle: absent, upserted, replaced in place, removed.
#[test]
fn store_round_trip() {
    let store = temp_store("round-trip");
    assert!(store.load().is_empty(), "a missing file is an empty list");

    store.upsert(sub("https://push.example.net/a")).unwrap();
    store.upsert(sub("https://push.example.net/b")).unwrap();
    assert_eq!(store.load().len(), 2);

    // An upsert on a known endpoint replaces rather than appends, so a page
    // reload that re-POSTs its subscription leaves one entry.
    let mut refreshed = sub("https://push.example.net/a");
    refreshed.auth = "AAAAAAAAAAAAAAAAAAAAAA".into();
    store.upsert(refreshed.clone()).unwrap();
    let loaded = store.load();
    assert_eq!(loaded.len(), 2, "replaced, not appended");
    assert!(loaded.contains(&refreshed));

    store.remove("https://push.example.net/a").unwrap();
    assert_eq!(
        store.load(),
        vec![sub("https://push.example.net/b")],
        "the other entry survives"
    );

    // Removing what is not there is not an error: an unsubscribe names an end
    // state, and a browser may unsubscribe twice.
    store.remove("https://push.example.net/a").unwrap();
    assert_eq!(store.load().len(), 1);

    let _ = std::fs::remove_file(&store.path);
}

/// A store file that is corrupt or unreadable reads as empty rather than
/// failing: a push that cannot go out must never be able to fail a turn.
#[test]
fn unparseable_store_reads_as_empty() {
    let store = temp_store("unparseable");
    std::fs::write(&store.path, "{ not json").unwrap();
    assert!(store.load().is_empty());
    let _ = std::fs::remove_file(&store.path);
}

/// The browser's `PushSubscription.toJSON()` shape is accepted verbatim,
/// including the `expirationTime` it carries and this store does not keep.
#[test]
fn browser_subscription_json_is_accepted_verbatim() {
    let json = r#"{
        "endpoint": "https://push.example.net/p/abc",
        "expirationTime": null,
        "keys": {
            "p256dh": "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
            "auth": "BTBZMqHH6r4Tts7J_aSIgg"
        }
    }"#;
    let parsed: Subscription = serde_json::from_str::<SubscriptionJson>(json)
        .expect("the browser's shape")
        .into();
    assert_eq!(parsed, sub("https://push.example.net/p/abc"));
}

/// The payload the service worker parses: the two lines it shows and the path a
/// click opens, which is `/` until a host fronts sessions.
#[test]
fn message_payload_shape() {
    let json =
        serde_json::to_value(Message::new("efference", "bash — writes outside the scope")).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "title": "efference",
            "body": "bash — writes outside the scope",
            "path": "/",
        })
    );
}
