//! The status bar's transport diagnostics: the retry in flight and the
//! upstream-failure chip (docs/transport.md).

use super::*;
use crate::api::{RetrySurface, UpstreamFailure, UpstreamRetry};
use crate::protocol::state::{Stall, StallReason};

fn retry(surface: RetrySurface, message: &str) -> TuiEvent {
    TuiEvent::Topic(TopicUpdate::UpstreamRetry(Some(UpstreamRetry {
        surface,
        attempt: 2,
        budget: 12,
        next_attempt_at: 0,
        message: message.into(),
        detail: String::new(),
    })))
}

fn failure(status: u16, count: u32) -> TuiEvent {
    let mut failure = UpstreamFailure::new(Some(status), "the upstream said no");
    failure.count = count;
    TuiEvent::Topic(TopicUpdate::UpstreamFailure(Some(failure)))
}

fn stalled(message: &str) -> TuiEvent {
    TuiEvent::Topic(TopicUpdate::Stall(Some(Stall {
        reason: StallReason::RetriesExhausted,
        attempts: Some(12),
        at: 0,
        message: message.into(),
        detail: String::new(),
    })))
}

/// A retry in flight is what the status bar leads with while the loop waits an
/// aborted stream out: the daemon's sentence verbatim, never the bare status
/// code. Both topics are level-triggered, so `None` on either is the all-clear
/// and removes its indicator.
#[test]
fn retry_and_failure_are_level_triggered_in_the_status_bar() {
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");
    play.phase(crate::protocol::state::LoopPhase::Requesting);

    // Idle: neither diagnostic adds chrome.
    assert!(play.find("retrying").is_none(), "nothing retrying at idle");
    assert!(play.find("upstream").is_none(), "no failure chip at idle");

    // A failed attempt sets both: the chip names the status, the retry says what
    // is being done about it and leads.
    play.event(failure(529, 2));
    play.event(retry(
        RetrySurface::Turn,
        "attempt 2 of 12 failed upstream, retrying",
    ));
    assert!(
        play.find("attempt 2 of 12 failed upstream, retrying")
            .is_some(),
        "the retry message renders verbatim"
    );
    assert!(
        play.find("upstream 529 ×2").is_some(),
        "chip still names it"
    );

    // The attempt that finalizes clears both topics — the chip disappears rather
    // than persisting as a survived failure.
    play.topic(TopicUpdate::UpstreamRetry(None));
    play.topic(TopicUpdate::UpstreamFailure(None));
    assert!(play.find("retrying").is_none(), "recovery clears the retry");
    assert!(
        play.find("upstream 529").is_none(),
        "recovery clears the chip"
    );
}

/// A turn retry: the message leads the status line, unprefixed.
#[test]
fn turn_retry_status_line() {
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");
    play.phase(crate::protocol::state::LoopPhase::Requesting);
    play.event(failure(529, 2));
    play.event(retry(
        RetrySurface::Turn,
        "attempt 2 of 12 failed upstream, retrying",
    ));
    play.snapshot("upstream_turn_retry");
}

/// A triage's retry carries a `triage:` prefix — the wait sits between tool
/// results, not in a generation, so the two are not the same event to an operator
/// watching a live run.
#[test]
fn triage_retry_status_line() {
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");
    play.phase(crate::protocol::state::LoopPhase::Requesting);
    play.event(retry(
        RetrySurface::Triage,
        "attempt 3 of 12 failed upstream, retrying",
    ));
    play.snapshot("upstream_triage_retry");
}

/// Exhaustion is where the two topics part: the retry clears (nothing is being
/// retried any more) and the `stall` topic takes its place, so an idle status bar
/// still says the trajectory stopped short. A turn starting clears it
/// (docs/transport.md).
#[test]
fn a_stalled_trajectory_stays_visible_at_idle() {
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");
    play.phase(crate::protocol::state::LoopPhase::Requesting);
    play.event(retry(
        RetrySurface::Turn,
        "attempt 11 of 12 failed upstream, retrying",
    ));

    // The budget is spent: the retry clears, the stall is set, the loop parks.
    play.topic(TopicUpdate::UpstreamRetry(None));
    play.event(stalled(
        "the trajectory stopped: 12 attempts in a row failed upstream",
    ));
    play.phase(crate::protocol::state::LoopPhase::Idle);
    assert!(play.find("retrying").is_none(), "nothing is being retried");
    assert!(
        play.find("the trajectory stopped").is_some(),
        "an idle daemon that gave up still says so"
    );

    // A trajectory starting is the all-clear.
    play.topic(TopicUpdate::Stall(None));
    play.phase(crate::protocol::state::LoopPhase::Requesting);
    assert!(
        play.find("the trajectory stopped").is_none(),
        "a running trajectory clears the stall"
    );
}
