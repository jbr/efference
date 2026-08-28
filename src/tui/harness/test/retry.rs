//! The status bar's transport diagnostics: the retry in flight and the
//! upstream-failure chip (docs/transport.md).

use super::*;
use crate::tui::{StallView, UpstreamFailureView, UpstreamRetryView};

fn retry(surface: &str, message: &str) -> TuiEvent {
    TuiEvent::UpstreamRetry(Some(UpstreamRetryView {
        surface: surface.into(),
        message: message.into(),
    }))
}

/// A retry in flight is what the status bar leads with while the loop waits an
/// aborted stream out: the daemon's sentence verbatim, never the bare status
/// code. Both topics are level-triggered, so `None` on either is the all-clear
/// and removes its indicator.
#[test]
fn retry_and_failure_are_level_triggered_in_the_status_bar() {
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");
    play.event(TuiEvent::Running(true));

    // Idle: neither diagnostic adds chrome.
    assert!(play.find("retrying").is_none(), "nothing retrying at idle");
    assert!(play.find("upstream").is_none(), "no failure chip at idle");

    // A failed attempt sets both: the chip names the status, the retry says what
    // is being done about it and leads.
    play.event(TuiEvent::UpstreamFailure(Some(UpstreamFailureView {
        status: 529,
        count: 2,
    })));
    play.event(retry("turn", "stream aborted, retrying (2 of 12)"));
    assert!(
        play.find("stream aborted, retrying (2 of 12)").is_some(),
        "the retry message renders verbatim"
    );
    assert!(
        play.find("upstream 529 ×2").is_some(),
        "chip still names it"
    );

    // The attempt that finalizes clears both topics — the chip disappears rather
    // than persisting as a survived failure.
    play.event(TuiEvent::UpstreamRetry(None));
    play.event(TuiEvent::UpstreamFailure(None));
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
    play.event(TuiEvent::Running(true));
    play.event(TuiEvent::UpstreamFailure(Some(UpstreamFailureView {
        status: 529,
        count: 2,
    })));
    play.event(retry("turn", "stream aborted, retrying (2 of 12)"));
    play.snapshot("upstream_turn_retry");
}

/// A fork peek's retry carries a `peek:` prefix — an exhausted peek withholds a
/// verdict where an exhausted turn ends the trajectory, so the two are not the
/// same event to an operator watching a live run.
#[test]
fn peek_retry_status_line() {
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");
    play.event(TuiEvent::Running(true));
    play.event(retry("peek", "stream aborted, retrying (3 of 12)"));
    play.snapshot("upstream_peek_retry");
}

/// Exhaustion is where the two topics part: the retry clears (nothing is being
/// retried any more) and the `stall` topic takes its place, so an idle status bar
/// still says the trajectory stopped short. A turn starting clears it
/// (docs/transport.md).
#[test]
fn a_stalled_trajectory_stays_visible_at_idle() {
    let mut play = Screenplay::new(80, 12);
    play.user("do a thing");
    play.event(TuiEvent::Running(true));
    play.event(retry("turn", "stream aborted, retrying (12 of 12)"));

    // The budget is spent: the retry clears, the stall is set, the loop parks.
    play.event(TuiEvent::UpstreamRetry(None));
    play.event(TuiEvent::Stall(Some(StallView {
        message: "the trajectory stopped: 12 attempts in a row aborted upstream \
                  — send a turn to retry"
            .into(),
    })));
    play.event(TuiEvent::Running(false));
    assert!(play.find("retrying").is_none(), "nothing is being retried");
    assert!(
        play.find("the trajectory stopped").is_some(),
        "an idle daemon that gave up still says so"
    );

    // A trajectory starting is the all-clear.
    play.event(TuiEvent::Stall(None));
    play.event(TuiEvent::Running(true));
    assert!(
        play.find("the trajectory stopped").is_none(),
        "a running trajectory clears the stall"
    );
}
