use crate::tui::parse::{parse_pending_gate, parse_upstream_failure};

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
    let body = r#"{"last_seq":1,"gate":{"call_id":"toolu_x","input":{"command":"ls"},"name":"bash"},"usage":{}}"#;
    let gate = parse_pending_gate(body).expect("a gate");
    assert_eq!(gate.call_id, "toolu_x");
    assert_eq!(gate.name, "bash");
    assert_eq!(gate.input["command"], "ls");
}

#[test]
fn null_gate_is_no_gate() {
    let body = r#"{"last_seq":7,"gate":null,"usage":{}}"#;
    assert!(parse_pending_gate(body).is_none());
}

#[test]
fn parses_upstream_failure() {
    let body = r#"{"upstream_failure":{"status":429,"count":2}}"#;
    let failure = parse_upstream_failure(body).expect("upstream failure");
    assert_eq!(failure.status, 429);
    assert_eq!(failure.count, 2);
}

#[test]
fn null_upstream_failure_is_absent() {
    assert!(parse_upstream_failure(r#"{"upstream_failure":null}"#).is_none());
}
