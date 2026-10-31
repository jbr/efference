//! The pending sign-ins (docs/web-bind.md, docs/state-store.md): what a code names and
//! for how long, what a claim collects and how often, and what the table keeps.

use super::*;
use std::time::Duration;

/// A throwaway store in a directory of its own, which goes when the returned guard drops.
fn scratch() -> (Store, tempfile::TempDir) {
    let dir = crate::testing::temp_dir("sign-ins");
    (Store::new(dir.path().join("state.db")), dir)
}

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

fn code(typed: &str) -> ShortCode {
    ShortCode::parse(typed).expect("a test code")
}

/// Every row, as `(code, label, joined, approved)` in insertion order — what the table
/// holds, read past the store's own filters.
fn rows(store: &Store) -> Vec<(String, String, String, bool)> {
    let db = store.connect().unwrap();
    let mut statement = db
        .prepare("SELECT code, label, joined, approved FROM pending_sign_ins ORDER BY rowid")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The whole flow at the store: waiting until approved, collected once after, and
/// nothing to collect or approve from then on.
#[test]
fn a_request_waits_for_approval_and_is_claimed_once() {
    let (store, _dir) = scratch();
    let requested = store.request("Safari on macOS", at(1_000)).unwrap();
    assert_eq!(
        store.claim(&requested.claim, at(1_001)).unwrap(),
        Claim::Waiting
    );
    assert_eq!(
        store.waiting(at(1_002)).unwrap(),
        [Pending {
            label: "Safari on macOS".into(),
            created_at: 1_000
        }]
    );

    assert_eq!(
        store.approve(&requested.code, at(1_003)).unwrap(),
        Some(Pending {
            label: "Safari on macOS".into(),
            created_at: 1_000
        })
    );
    assert!(
        store.waiting(at(1_003)).unwrap().is_empty(),
        "an approved sign-in waits for nothing"
    );
    assert_eq!(
        store.claim(&requested.claim, at(1_004)).unwrap(),
        Claim::Approved {
            label: "Safari on macOS".into(),
            joined: Joined::Code
        }
    );
    assert_eq!(
        store.claim(&requested.claim, at(1_005)).unwrap(),
        Claim::Unknown,
        "a second claim collects nothing"
    );
    assert_eq!(
        store.approve(&requested.code, at(1_005)).unwrap(),
        None,
        "and the code names nothing"
    );
    assert!(rows(&store).is_empty(), "the claim deleted the row");
}

/// Approval is once: a code already approved is not approved again, so a second
/// `approve` of the same line says so rather than reporting a second success.
#[test]
fn a_code_is_approved_once() {
    let (store, _dir) = scratch();
    let requested = store.request("phone", at(1_000)).unwrap();
    assert!(store.approve(&requested.code, at(1_001)).unwrap().is_some());
    assert_eq!(store.approve(&requested.code, at(1_002)).unwrap(), None);
}

/// A code that names no live sign-in approves nothing, and leaves the waiting one
/// waiting.
#[test]
fn a_code_naming_no_live_sign_in_is_refused() {
    let (store, _dir) = scratch();
    let requested = store.request("phone", at(1_000)).unwrap();
    let other = (0..)
        .map(|_| ShortCode::random())
        .find(|other| *other != requested.code)
        .unwrap();
    assert_eq!(store.approve(&other, at(1_001)).unwrap(), None);
    assert_eq!(
        store.claim(&requested.claim, at(1_002)).unwrap(),
        Claim::Waiting
    );
    assert_eq!(
        store.approve(&code("ABC-DEF"), at(1_002)).unwrap(),
        None,
        "nor does one nothing ever held"
    );
}

/// The TTL is checked at approval: the last second inside it approves, the first one
/// past it does not.
#[test]
fn the_ttl_bounds_approval() {
    let (store, _dir) = scratch();
    let late = store.request("late", at(1_000)).unwrap();
    let inside = store.request("inside", at(1_000)).unwrap();
    let expiry = 1_000 + TTL_SECS as u64;
    assert!(
        store
            .approve(&inside.code, at(expiry - 1))
            .unwrap()
            .is_some()
    );
    assert_eq!(store.approve(&late.code, at(expiry)).unwrap(), None);
    assert_eq!(
        store.claim(&late.claim, at(expiry)).unwrap(),
        Claim::Unknown
    );
}

/// The TTL is checked at claim as well: an approved sign-in collects inside it and is
/// gone after it.
#[test]
fn the_ttl_bounds_the_claim() {
    let (store, _dir) = scratch();
    let inside = store.request("inside", at(1_000)).unwrap();
    let late = store.request("late", at(1_000)).unwrap();
    store.approve(&inside.code, at(1_010)).unwrap().unwrap();
    store.approve(&late.code, at(1_010)).unwrap().unwrap();
    let expiry = 1_000 + TTL_SECS as u64;
    assert!(matches!(
        store.claim(&inside.claim, at(expiry - 1)).unwrap(),
        Claim::Approved { .. }
    ));
    assert_eq!(
        store.claim(&late.claim, at(expiry)).unwrap(),
        Claim::Unknown
    );
    let waiting = store.request("waiting", at(2_000)).unwrap();
    assert_eq!(
        store
            .claim(&waiting.claim, at(2_000 + TTL_SECS as u64))
            .unwrap(),
        Claim::Unknown,
        "an unapproved one expires the same way"
    );
}

/// Approval near the end of the TTL leaves the page time to collect: the row stays
/// claimable for at least [`CLAIM_GRACE_SECS`] after it was approved.
#[test]
fn an_approval_in_the_last_second_can_still_be_claimed() {
    let (store, _dir) = scratch();
    let request = store.request("late", at(1_000)).unwrap();
    let expiry = 1_000 + TTL_SECS as u64;
    store
        .approve(&request.code, at(expiry - 1))
        .unwrap()
        .unwrap();
    assert!(matches!(
        store.claim(&request.claim, at(expiry + 1)).unwrap(),
        Claim::Approved { .. }
    ));
    let late = store.request("later", at(2_000)).unwrap();
    store
        .approve(&late.code, at(2_000 + TTL_SECS as u64 - 1))
        .unwrap()
        .unwrap();
    let grace_end = 2_000 + TTL_SECS as u64 - 1 + CLAIM_GRACE_SECS as u64;
    assert_eq!(
        store.claim(&late.claim, at(grace_end)).unwrap(),
        Claim::Unknown,
        "the grace is bounded too"
    );
}

/// A ninth waiting request evicts the oldest: its claim names nothing, and the eight
/// after it wait on.
#[test]
fn the_ninth_request_evicts_the_oldest() {
    let (store, _dir) = scratch();
    let requests: Vec<Requested> = (0..WAITING_CAP as u64 + 1)
        .map(|n| store.request(&format!("tab {n}"), at(1_000 + n)).unwrap())
        .collect();
    assert_eq!(
        store.claim(&requests[0].claim, at(1_020)).unwrap(),
        Claim::Unknown
    );
    assert_eq!(store.approve(&requests[0].code, at(1_020)).unwrap(), None);
    for request in &requests[1..] {
        assert_eq!(
            store.claim(&request.claim, at(1_020)).unwrap(),
            Claim::Waiting
        );
    }
    let labels: Vec<String> = store
        .waiting(at(1_020))
        .unwrap()
        .into_iter()
        .map(|pending| pending.label)
        .collect();
    assert_eq!(
        labels,
        (1..=WAITING_CAP)
            .map(|n| format!("tab {n}"))
            .collect::<Vec<_>>(),
        "oldest first, and the evicted one gone"
    );
}

/// What a peer churning `/pair` cannot displace: a link minted at the terminal, and a
/// request the operator already approved. Neither waits for approval, so neither counts
/// against the cap nor is evicted by it.
#[test]
fn requests_never_evict_an_approved_sign_in() {
    let (store, _dir) = scratch();
    let link = store.approved_link(at(1_000)).unwrap();
    let approved = store.request("approved", at(1_000)).unwrap();
    store.approve(&approved.code, at(1_001)).unwrap().unwrap();
    for n in 0..3 * WAITING_CAP as u64 {
        store.request("churn", at(1_002 + n)).unwrap();
    }
    assert_eq!(
        store.waiting(at(1_050)).unwrap().len(),
        WAITING_CAP as usize
    );
    assert_eq!(
        store.claim(&link, at(1_050)).unwrap(),
        Claim::Approved {
            label: "browser".into(),
            joined: Joined::Link
        }
    );
    assert_eq!(
        store.claim(&approved.claim, at(1_050)).unwrap(),
        Claim::Approved {
            label: "approved".into(),
            joined: Joined::Code
        }
    );
}

/// A link's sign-in is approved before anything asks, joins as `link`, and collects
/// once.
#[test]
fn a_link_is_a_sign_in_approved_in_advance() {
    let (store, _dir) = scratch();
    let link = store.approved_link(at(1_000)).unwrap();
    assert!(
        store.waiting(at(1_000)).unwrap().is_empty(),
        "a link waits for no approval"
    );
    assert_eq!(
        store.claim(&link, at(1_001)).unwrap(),
        Claim::Approved {
            label: "browser".into(),
            joined: Joined::Link
        }
    );
    assert_eq!(store.claim(&link, at(1_002)).unwrap(), Claim::Unknown);
}

/// Codes are unique among the live sign-ins: a draw that collides with a live code is
/// drawn again.
#[test]
fn a_colliding_code_is_drawn_again() {
    let (store, _dir) = scratch();
    let first = store
        .insert("first", Joined::Code, at(1_000), || code("KQ7-M4X"))
        .unwrap();
    let mut draws = [code("KQ7-M4X"), code("KQ7-M4X"), code("ZZZ-222")].into_iter();
    let second = store
        .insert("second", Joined::Code, at(1_001), || draws.next().unwrap())
        .unwrap();
    assert_eq!(first.code, code("KQ7-M4X"));
    assert_eq!(second.code, code("ZZZ-222"));
    assert_eq!(draws.next(), None, "both collisions were drawn again");
    assert_eq!(
        store
            .approve(&code("ZZZ222"), at(1_002))
            .unwrap()
            .unwrap()
            .label,
        "second"
    );

    // An expired row's code is free again: the insert prunes before it draws.
    let later = store
        .insert("later", Joined::Code, at(1_000 + TTL_SECS as u64), || {
            code("KQ7-M4X")
        })
        .unwrap();
    assert_eq!(later.code, code("KQ7-M4X"));
}

/// A code is read the way a person types it: either case, the dash optional, spaces
/// ignored; anything else is no code at all.
#[test]
fn codes_are_normalized() {
    let shown = code("KQ7-M4X");
    assert_eq!(shown.to_string(), "KQ7-M4X");
    for typed in ["kq7-m4x", "KQ7M4X", " kq7 m4x\n", "K-Q-7-M-4-X", "Kq7-M4x"] {
        assert_eq!(ShortCode::parse(typed), Some(shown.clone()), "{typed:?}");
    }
    for refused in [
        "",
        "KQ7-M4",
        "KQ7-M4XX",
        "KQ0-M4X",
        "KQ7-M4O",
        "KQ1-M4X",
        "KQI-M4X",
        "KQL-M4X",
        "KQU-M4X",
        "KQ7_M4X",
        "KQ7-M4É",
        "KQ7–M4X",
    ] {
        assert_eq!(ShortCode::parse(refused), None, "{refused:?}");
    }
    for _ in 0..200 {
        let drawn = ShortCode::random();
        assert_eq!(ShortCode::parse(&drawn.to_string()), Some(drawn.clone()));
        assert!(drawn.0.iter().all(|c| ALPHABET.contains(c)));
    }
}

/// The waiting list names what asked and when, and carries no code and no claim: the
/// approving side types the code the browser shows, never picks it from a list.
#[test]
fn the_waiting_list_never_carries_a_code() {
    let (store, _dir) = scratch();
    let requested = store.request("Firefox on Linux", at(1_000)).unwrap();
    let listed = serde_json::to_string(&store.waiting(at(1_001)).unwrap()).unwrap();
    assert_eq!(
        listed,
        r#"[{"label":"Firefox on Linux","created_at":1000}]"#
    );
    assert!(!listed.contains(requested.code.as_str()), "{listed}");
    assert!(!listed.contains(&requested.claim.encoded()), "{listed}");
    let approved =
        serde_json::to_string(&store.approve(&requested.code, at(1_002)).unwrap()).unwrap();
    assert!(!approved.contains(requested.code.as_str()), "{approved}");
}

/// The table keys a row by the claim secret's SHA-256 and holds the secret nowhere, so
/// reading the database collects no token.
#[test]
fn the_claim_secret_is_stored_as_its_digest() {
    let (store, _dir) = scratch();
    let requested = store.request("phone", at(1_000)).unwrap();
    let db = store.connect().unwrap();
    let stored: Vec<u8> = db
        .query_row("SELECT claim FROM pending_sign_ins", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored, Sha256::digest(requested.claim.0).to_vec());
    let dump: String = db
        .query_row(
            "SELECT quote(claim) || code || label FROM pending_sign_ins",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let hex: String = requested
        .claim
        .0
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    assert!(!dump.contains(&hex), "{dump}");
    assert!(!dump.contains(&requested.claim.encoded()), "{dump}");
}

/// A claim secret is 22 characters of base64url; anything else names nothing.
#[test]
fn a_claim_secret_has_one_spelling() {
    let secret = ClaimSecret::random();
    let encoded = secret.encoded();
    assert_eq!(encoded.len(), 22, "{encoded}");
    assert_eq!(ClaimSecret::parse(&encoded).unwrap().0, secret.0);
    for refused in [
        String::new(),
        encoded[..21].to_string(),
        format!("{encoded}A"),
        format!("{encoded}=="),
        encoded.replace(&encoded[..1], "+"),
        "*".repeat(22),
    ] {
        assert!(ClaimSecret::parse(&refused).is_none(), "{refused:?}");
    }
}

/// Expired rows go at every write, so the table does not keep what nothing can use.
#[test]
fn expired_rows_are_pruned_by_every_write() {
    let (store, _dir) = scratch();
    store.request("old", at(1_000)).unwrap();
    store.approved_link(at(1_000)).unwrap();
    let past = 1_000 + TTL_SECS as u64;
    store.request("new", at(past)).unwrap();
    assert_eq!(
        rows(&store)
            .into_iter()
            .map(|(_, label, _, _)| label)
            .collect::<Vec<_>>(),
        ["new"]
    );

    let stale = store.request("stale", at(past)).unwrap();
    let much_later = at(past + TTL_SECS as u64);
    assert_eq!(store.approve(&stale.code, much_later).unwrap(), None);
    assert!(rows(&store).is_empty(), "approval prunes too");
    store.request("again", at(5_000)).unwrap();
    let ghost = ClaimSecret::random();
    assert_eq!(
        store.claim(&ghost, at(5_000 + TTL_SECS as u64)).unwrap(),
        Claim::Unknown
    );
    assert!(rows(&store).is_empty(), "and so does a claim");
}

/// Labels are requester-supplied text bound for the operator's terminal, cleaned on
/// the way in as a device's are; an empty one is `browser`.
#[test]
fn labels_are_cleaned_on_the_way_in() {
    let (store, _dir) = scratch();
    store
        .request("\u{1b}[2JSafari\u{202E} on\nmacOS", at(1_000))
        .unwrap();
    store.request(" ", at(1_001)).unwrap();
    let labels: Vec<String> = store
        .waiting(at(1_002))
        .unwrap()
        .into_iter()
        .map(|pending| pending.label)
        .collect();
    assert_eq!(labels, ["Safari on macOS", "browser"]);
}

/// Rotation's half: every pending sign-in goes, approved or not.
#[test]
fn clear_empties_the_table() {
    let (store, _dir) = scratch();
    let link = store.approved_link(at(1_000)).unwrap();
    store.request("phone", at(1_000)).unwrap();
    assert_eq!(store.clear().unwrap(), 2);
    assert_eq!(store.claim(&link, at(1_001)).unwrap(), Claim::Unknown);
    assert_eq!(store.clear().unwrap(), 0);
}

/// The table earlier builds kept TOTP burns in is dropped when this one opens, and
/// opening again with it gone is no error.
#[test]
fn opening_drops_the_totp_burns_table() {
    let (store, _dir) = scratch();
    crate::state::open(
        &store.path,
        "CREATE TABLE IF NOT EXISTS totp_burns (step INTEGER PRIMARY KEY);
         INSERT INTO totp_burns (step) VALUES (1000);",
    )
    .unwrap();
    assert!(store.waiting(at(1)).unwrap().is_empty());
    let tables: Vec<String> = store
        .connect()
        .unwrap()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(tables, ["pending_sign_ins"]);
}

/// A table that cannot be read is an error to every operation, never an empty answer:
/// a claim must not read as unknown, nor a request as recorded.
#[test]
fn a_file_that_is_not_a_database_is_an_error() {
    let (store, _dir) = scratch();
    std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
    std::fs::write(&store.path, "{ not a database").unwrap();
    assert!(store.request("phone", at(1)).is_err());
    assert!(store.approved_link(at(1)).is_err());
    assert!(store.approve(&code("KQ7-M4X"), at(1)).is_err());
    assert!(store.claim(&ClaimSecret::random(), at(1)).is_err());
    assert!(store.waiting(at(1)).is_err());
}
