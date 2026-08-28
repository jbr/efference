use crate::tui::UpstreamRetryView;
use crate::tui::parse::{
    parse_pending_gate, parse_session_identity, parse_upstream_failure, parse_upstream_retry,
};
use crate::tui::state::upstream_retry_indicator;

use crate::tui::run::{DEFAULT_THEME, resolve_theme};

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
#[test]
fn parses_a_pending_gate() {
    // A `gate` state frame's `data`: the topic's whole value.
    let topic = serde_json::json!({"call_id":"toolu_x","input":{"command":"ls"},"name":"bash"});
    let gate = parse_pending_gate(&topic).expect("a gate");
    assert_eq!(gate.call_id, "toolu_x");
    assert_eq!(gate.name, "bash");
    assert_eq!(gate.input["command"], "ls");
}

#[test]
fn null_gate_is_no_gate() {
    // `null` is the topic's cleared value (docs/control-api.md), not an absence.
    assert!(parse_pending_gate(&serde_json::Value::Null).is_none());
}

#[test]
fn parses_upstream_failure() {
    let topic = serde_json::json!({"status":429,"count":2});
    let failure = parse_upstream_failure(&topic).expect("upstream failure");
    assert_eq!(failure.status, 429);
    assert_eq!(failure.count, 2);
}

#[test]
fn null_upstream_failure_is_absent() {
    assert!(parse_upstream_failure(&serde_json::Value::Null).is_none());
}

#[test]
fn parses_upstream_retry() {
    let topic = serde_json::json!({
        "surface": "peek",
        "attempt": 2,
        "budget": 12,
        "next_attempt_at": 1754300002000u64,
        "message": "stream aborted, retrying (2 of 12)",
        "detail": "upstream SSE stream failed mid-stream: connection reset",
    });
    let retry = parse_upstream_retry(&topic).expect("upstream retry");
    assert_eq!(retry.surface, "peek");
    assert_eq!(retry.message, "stream aborted, retrying (2 of 12)");
}

#[test]
fn null_upstream_retry_is_the_all_clear() {
    assert!(parse_upstream_retry(&serde_json::Value::Null).is_none());
}

/// The indicator leads with the daemon's own sentence, never the status code, and
/// marks a fork peek's retry apart from a turn's (docs/transport.md).
#[test]
fn retry_indicator_names_the_surface() {
    let view = |surface: &str| UpstreamRetryView {
        surface: surface.into(),
        message: "stream aborted, retrying (2 of 12)".into(),
    };
    assert_eq!(
        upstream_retry_indicator(Some(&view("turn"))).unwrap(),
        "↻ stream aborted, retrying (2 of 12)"
    );
    assert_eq!(
        upstream_retry_indicator(Some(&view("peek"))).unwrap(),
        "↻ peek: stream aborted, retrying (2 of 12)"
    );
    assert!(upstream_retry_indicator(None).is_none());
}

/// The `session_identity` topic names the model and the working directory
/// (docs/control-api.md). The cwd is shortened against `$HOME` at parse time, so
/// the renderer never reads the environment.
#[test]
fn parses_session_identity_with_a_home_relative_cwd() {
    let home = std::env::home_dir().expect("a home directory");
    let cwd = home.join("code/efference");
    let topic = serde_json::json!({
        "model": "anthropic/claude-opus-4",
        "cwd": cwd.to_string_lossy(),
    });
    let identity = parse_session_identity(&topic).expect("session identity");
    assert_eq!(identity.model, "anthropic/claude-opus-4");
    assert_eq!(identity.cwd, "~/code/efference");

    // A path outside home keeps its absolute form — a partial component match
    // ("/home/jbrx") must not be rewritten either.
    let elsewhere = serde_json::json!({ "model": "m", "cwd": "/srv/build" });
    assert_eq!(
        parse_session_identity(&elsewhere).unwrap().cwd,
        "/srv/build"
    );
}

/// A daemon that never set the topic reports `null`, which is no view at all —
/// the sidebar's SESSION section then simply doesn't render.
#[test]
fn null_session_identity_is_no_view() {
    assert!(parse_session_identity(&serde_json::Value::Null).is_none());
}
