use crate::clients::tui::state::{
    stall_indicator, upstream_failure_indicator, upstream_retry_indicator,
};
use crate::inference::api::{RetrySurface, UpstreamFailure, UpstreamRetry};
use crate::protocol::state::{Stall, StallReason};

use crate::clients::tui::run::{DEFAULT_THEME, resolve_theme};

#[test]
fn default_prefers_catppuccin_frappe() {
    // The submodule theme set includes it, so the no-override default is it
    // (not the sort-first "1337").
    assert_eq!(resolve_theme(None).unwrap(), DEFAULT_THEME);
}

#[test]
fn a_known_name_passes_through() {
    assert_eq!(resolve_theme(Some("Nord".into())).unwrap(), "Nord");
}

#[test]
fn an_unknown_name_is_an_error() {
    assert!(resolve_theme(Some("definitely-not-a-theme".into())).is_err());
}

#[test]
fn a_tmtheme_path_is_accepted_without_membership() {
    let p = "/tmp/Custom.tmTheme";
    assert_eq!(resolve_theme(Some(p.into())).unwrap(), p);
}
/// The failure indicator names the upstream's status and how many times in a row it came
/// back. A failure the upstream gave no status for names nothing, and the bar shows none.
#[test]
fn failure_indicator_names_the_status_and_the_count() {
    let mut failure = UpstreamFailure::new(Some(429), "slow down");
    failure.count = 2;
    assert_eq!(
        upstream_failure_indicator(Some(&failure)).unwrap(),
        "upstream 429 ×2"
    );
    assert!(
        upstream_failure_indicator(Some(&UpstreamFailure::new(None, "stream reset"))).is_none()
    );
    assert!(upstream_failure_indicator(None).is_none());
}

/// The indicator leads with the daemon's own sentence, never the status code, marks
/// a triage retry apart from a turn's, and says `⏎` retries now (docs/transport.md).
#[test]
fn retry_indicator_names_the_surface() {
    let view = |surface| UpstreamRetry {
        surface,
        attempt: 2,
        budget: 12,
        next_attempt_at: 1_754_300_002_000,
        message: "attempt 2 of 12 failed upstream, retrying".into(),
        detail: "upstream SSE stream failed mid-stream: connection reset".into(),
    };
    assert_eq!(
        upstream_retry_indicator(Some(&view(RetrySurface::Turn))).unwrap(),
        "↻ attempt 2 of 12 failed upstream, retrying · ⏎ retries now"
    );
    assert_eq!(
        upstream_retry_indicator(Some(&view(RetrySurface::Triage))).unwrap(),
        "↻ triage: attempt 2 of 12 failed upstream, retrying · ⏎ retries now"
    );
    assert!(upstream_retry_indicator(None).is_none());
}

/// The `stall` topic is what tells an idle TUI that the last trajectory gave up
/// rather than finished (docs/transport.md). Only the lead line is shown; it renders
/// verbatim with the recovery gesture; `null` — a trajectory running again — is no indicator.
#[test]
fn stall_indicator_shows_the_daemons_lead_line() {
    let stall = Stall {
        reason: StallReason::RetriesExhausted,
        attempts: Some(12),
        at: 1_754_300_002_000,
        message: "the trajectory stopped: 12 attempts in a row failed upstream".into(),
        detail: "upstream SSE stream failed mid-stream: connection reset".into(),
    };
    assert_eq!(
        stall_indicator(Some(&stall)).unwrap(),
        "⚠ the trajectory stopped: 12 attempts in a row failed upstream · ⏎ to retry"
    );
    assert!(stall_indicator(None).is_none());
}
