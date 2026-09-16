use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::env_table::Masker;

fn counting(values: Vec<Result<String, String>>) -> (AuthValue, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let initial = "old".to_string();
    let value = AuthValue::reloadable(
        initial,
        Box::new(move || {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            values.get(n).cloned().unwrap_or(Ok("old".into()))
        }),
    );
    (value, calls)
}

#[test]
fn a_literal_has_no_source_and_compares_as_its_value() {
    let value = AuthValue::from("Bearer k");
    assert_eq!(value.get(), "Bearer k");
    assert_eq!(value, "Bearer k");
}

#[tokio::test]
async fn a_literal_reports_that_it_cannot_be_re_read() {
    assert_eq!(AuthValue::from("k").reload("k").await, Reloaded::NoSource);
}

#[tokio::test]
async fn a_changed_source_replaces_the_value() {
    let (value, calls) = counting(vec![Ok("new".into())]);
    assert_eq!(value.reload("old").await, Reloaded::Replaced);
    assert_eq!(value.get(), "new");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_unchanged_source_leaves_the_value_and_says_so() {
    let (value, _) = counting(vec![Ok("old".into())]);
    assert_eq!(value.reload("old").await, Reloaded::Unchanged);
    assert_eq!(value.get(), "old");
}

#[tokio::test]
async fn an_unreadable_source_carries_its_own_words() {
    let (value, _) = counting(vec![Err("no auth.json".into())]);
    assert_eq!(
        value.reload("old").await,
        Reloaded::Failed("no auth.json".into())
    );
    assert_eq!(value.get(), "old");
}

/// The seam's whole point: a session holds a clone, and replacing the value at
/// the connection is visible through it without rebuilding anything.
#[tokio::test]
async fn a_clone_reads_through_to_the_replacement() {
    let (connection, _) = counting(vec![Ok("new".into())]);
    let session = connection.clone();
    assert_eq!(session.get(), "old");
    connection.reload("old").await;
    assert_eq!(session.get(), "new");
}

/// Several sessions failing on the same token cost one read: whoever gets there
/// second finds the value already past what it sent, and retries on it.
#[tokio::test]
async fn failures_on_one_token_read_the_source_once() {
    let (value, calls) = counting(vec![Ok("new".into())]);
    let other = value.clone();
    assert_eq!(value.reload("old").await, Reloaded::Replaced);
    assert_eq!(other.reload("old").await, Reloaded::Replaced);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(value.get(), "new");
}

/// A caller that sent the value now in place is the one that actually needs the
/// source read, however many reloads came before it.
#[tokio::test]
async fn a_failure_on_the_current_value_does_read_the_source() {
    let (value, calls) = counting(vec![Ok("new".into()), Ok("newer".into())]);
    assert_eq!(value.reload("old").await, Reloaded::Replaced);
    assert_eq!(value.reload("new").await, Reloaded::Replaced);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(value.get(), "newer");
}

#[test]
fn the_value_is_never_debug_printed() {
    assert_eq!(format!("{:?}", AuthValue::from("sk-secret")), "[redacted]");
}

/// A replacement is masked wherever the value it replaces was (docs/secrets.md).
/// Without this the credential that arrives mid-session is the one that is *not*
/// redacted, which inverts the protection.
#[tokio::test]
async fn a_replacement_is_masked_in_the_sessions_holding_it() {
    let masker = Arc::new(Masker::default());
    masker.absorb_secret(&[("codex".into(), "old".into())]);
    let (value, _) = counting(vec![Ok("new".into())]);
    value.mask_replacements_in("codex", &masker);

    assert_eq!(value.reload("old").await, Reloaded::Replaced);
    assert!(
        !masker.mask("token new here").contains("new"),
        "the replacement is redacted: {}",
        masker.mask("token new here")
    );
    assert!(
        !masker.mask("token old here").contains("old"),
        "and the value it replaced stays redacted"
    );
}

/// The cell outlives the sessions using it, so a dead session's masker must not
/// be retained by it.
#[tokio::test]
async fn a_masker_is_held_weakly() {
    let (value, _) = counting(vec![Ok("new".into())]);
    let masker = Arc::new(Masker::default());
    value.mask_replacements_in("codex", &masker);
    let weak = Arc::downgrade(&masker);
    drop(masker);
    assert_eq!(weak.strong_count(), 0, "nothing keeps it alive");
    assert_eq!(value.reload("old").await, Reloaded::Replaced);
}
