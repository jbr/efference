//! The wait the other tests use (`daemon/wait.rs`): each signal wakes it, nothing polls, and a
//! wait that cannot succeed fails with where the session stood. The clock is paused, so the
//! only timer in a wait is the hang guard: a wake that did not come from a signal would end
//! the test at the guard.

use super::*;
use crate::daemon::{Host, LoopPhase};
use crate::inference::api::RetrySurface;

/// Run `wake` on another task, after the waiter has had its first look, and wait for the
/// session to hold what `wake` does.
async fn woken_by(wake: impl FnOnce(&Session) + Send + 'static, ready: impl Fn(&Session) -> bool) {
    let (session, _rx) = Session::in_memory();
    let waker = Arc::clone(&session);
    let task = tokio::spawn(async move {
        tokio::task::yield_now().await;
        wake(&waker);
    });
    session.wait_until("the wake", || ready(&session)).await;
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_predicate_that_already_holds_returns_without_waiting() {
    let (session, _rx) = Session::in_memory();
    session.wait_until("nothing", || true).await;
}

#[tokio::test(start_paused = true)]
async fn a_log_append_wakes_the_wait() {
    woken_by(
        |session| {
            session.append(1, Payload::user("hi")).unwrap();
        },
        |session| !session.snapshot().is_empty(),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_phase_change_wakes_the_wait() {
    woken_by(
        |session| {
            session.set_phase(LoopPhase::Requesting);
        },
        |session| session.loop_phase() == LoopPhase::Requesting,
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_state_ping_wakes_the_wait() {
    let (session, _rx) = Session::in_memory();
    let flag = Arc::new(AtomicBool::new(false));
    let waker = Arc::clone(&session);
    let raised = Arc::clone(&flag);
    let task = tokio::spawn(async move {
        tokio::task::yield_now().await;
        raised.store(true, Ordering::SeqCst);
        waker.touch_state();
    });
    session
        .wait_until("the ping", || flag.load(Ordering::SeqCst))
        .await;
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_retry_change_wakes_the_wait() {
    woken_by(
        |session| {
            session
                .upstream_retry_for_test()
                .set(crate::inference::api::stream_abort_retry(
                    RetrySurface::Turn,
                    1,
                    3,
                    Duration::from_secs(1),
                    "stream aborted",
                ));
        },
        |session| session.upstream_retry_for_test().get().is_some(),
    )
    .await;
}

/// The host's wait wakes on the listing, which a session leaving it signals.
#[tokio::test(start_paused = true)]
async fn the_hosts_wait_wakes_on_the_listing() {
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    let flag = Arc::new(AtomicBool::new(false));
    let waker = Arc::clone(&host);
    let raised = Arc::clone(&flag);
    let task = tokio::spawn(async move {
        tokio::task::yield_now().await;
        raised.store(true, Ordering::SeqCst);
        waker.touch_listing();
    });
    host.wait_until("the listing", || flag.load(Ordering::SeqCst))
        .await;
    task.await.unwrap();
}

/// A predicate no signal can make true fails at the hang guard, and the failure says where
/// the session was: its phase, its queue, the call it holds, and the end of its log.
#[tokio::test(start_paused = true)]
async fn a_wait_that_cannot_succeed_fails_with_where_the_session_stood() {
    let (session, _rx) = Session::in_memory();
    session.append(1, Payload::user("first words")).unwrap();
    session.submit_turn(SubmittedTurn::new("queued words", "s1"));
    let _gate = session.register_pending(&bash_call("echo held"), Default::default());
    let task = tokio::spawn(async move {
        session
            .wait_until("a thing that never happens", || false)
            .await;
    });
    let panic = task.await.expect_err("the wait gave up").into_panic();
    let message = panic.downcast_ref::<String>().expect("a formatted message");
    for expected in [
        "timed out after 60s waiting: a thing that never happens",
        "phase: Gated",
        r#"queue: ["queued words"]"#,
        "pending gate: Some(\"bash (toolu_x) ",
        "echo held",
        "first words",
    ] {
        assert!(message.contains(expected), "{expected:?} in {message}");
    }
}
