//! The status bar's transport diagnostics: the retry in flight and the
//! upstream-failure chip (docs/transport.md).

use super::*;
use crate::tui::{UpstreamFailureView, UpstreamRetryView};

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
