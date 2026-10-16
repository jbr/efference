use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use crate::env_table::Masker;

fn counting(values: Vec<Result<String, String>>) -> (AuthValue, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let initial = "old".to_string();
    let value = AuthValue::reloadable(
        initial,
        sync_reload(move || {
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

/// The interface's whole point: a session holds a clone, and replacing the value at
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

/// A value whose source states an expiry, re-read through `results` in order.
fn renewing(
    renew_at: Option<SystemTime>,
    results: Vec<Result<Reread, String>>,
) -> (AuthValue, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let value = AuthValue::renewing(
        "old".into(),
        renew_at,
        Box::new(move || {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            let result = results[n].clone();
            Box::pin(async move { result })
        }),
    );
    (value, calls)
}

fn in_a_day() -> Option<SystemTime> {
    Some(SystemTime::now() + Duration::from_secs(60 * 60 * 24))
}

fn fresh(value: &str, renew_at: Option<SystemTime>) -> Result<Reread, String> {
    Ok(Reread {
        value: value.into(),
        renew_at,
    })
}

/// The case `efference dev models` hit: a listing probe sent the value loaded
/// at launch, which had passed its expiry, with nothing to renew it.
#[tokio::test]
async fn a_value_past_its_deadline_is_renewed_before_it_is_sent() {
    let next = in_a_day();
    let (value, calls) = renewing(Some(SystemTime::UNIX_EPOCH), vec![fresh("new", next)]);
    assert_eq!(value.current().await, "new");
    assert_eq!(value.renew_at(), next);
    assert_eq!(value.current().await, "new");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "renewed once, then current"
    );
}

#[tokio::test]
async fn a_value_before_its_deadline_is_sent_without_a_read() {
    let (value, calls) = renewing(in_a_day(), vec![]);
    assert_eq!(value.current().await, "old");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// The old value may still be accepted until its real expiry, so a failed
/// renewal sends it anyway — and waits before trying again rather than asking
/// the issuer on every request.
#[tokio::test]
async fn a_failed_renewal_keeps_the_value_and_backs_off() {
    let (value, calls) = renewing(Some(SystemTime::UNIX_EPOCH), vec![Err("refused".into())]);
    assert_eq!(value.current().await, "old");
    assert!(value.renew_at().unwrap() > SystemTime::now());
    assert_eq!(value.current().await, "old");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// What tests a stored refresh token at host launch: a value already due is
/// renewed without waiting, and the loop ends when the source stops stating a
/// deadline.
#[tokio::test]
async fn keeping_a_due_value_renewed_renews_it_at_once() {
    let (value, calls) = renewing(Some(SystemTime::UNIX_EPOCH), vec![fresh("new", None)]);
    value.clone().keep_renewed().await;
    assert_eq!(value.get(), "new");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
