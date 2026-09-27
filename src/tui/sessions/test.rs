use super::*;
use crate::tui::harness::Screenplay;
use crossterm::event::{KeyCode, KeyModifiers};

fn row(id: &str, status: &str, processes: usize) -> SessionSummary {
    SessionSummary {
        id: id.into(),
        title: Some(format!("{id}'s work")),
        cwd: Some("/work".into()),
        created_by: None,
        status: status.into(),
        processes,
    }
}

/// The confirm rule (docs/tui.md): an idle session with nothing running is archived
/// on the spot, and anything else says what would be lost.
#[test]
fn only_a_session_with_something_to_lose_is_confirmed() {
    assert_eq!(needs_confirm(&row("quiet", "idle", 0)), None);
    for (status, processes, expected) in [
        ("running", 0, "busy is mid-turn"),
        ("gated", 0, "busy is holding a call at a gate"),
        ("stalled", 0, "busy stalled and has not been answered"),
        ("idle", 1, "busy holds 1 background process"),
        ("idle", 3, "busy holds 3 background processes"),
        (
            "running",
            2,
            "busy is mid-turn and holds 2 background processes",
        ),
    ] {
        assert_eq!(
            needs_confirm(&row("busy", status, processes)).as_deref(),
            Some(expected)
        );
    }
}

/// Opening the switcher asks the host for the listing; the listing lands and the
/// rows are what the keys act on.
#[test]
fn opening_the_switcher_asks_for_the_listing() {
    let mut play = Screenplay::new(90, 24);
    play.state.open_sessions();
    assert!(matches!(
        play.took_host_cmd(),
        Some(crate::tui::HostCmd::List)
    ));
    play.event(crate::tui::TuiEvent::Sessions(Ok(vec![
        row("alpha", "idle", 0),
        row("beta", "running", 0),
    ])));
    let screen = play.render();
    assert!(screen.contains("alpha"), "{screen}");
    assert!(screen.contains("beta"), "{screen}");
    assert!(screen.contains("running"), "{screen}");
}

/// `a` on an idle row archives it with no ceremony; `a` on a busy one asks first,
/// and only `y` goes through.
#[test]
fn archiving_asks_only_when_there_is_something_to_lose() {
    let mut play = Screenplay::new(90, 24);
    play.state.open_sessions();
    let _ = play.took_host_cmd();
    play.event(crate::tui::TuiEvent::Sessions(Ok(vec![
        row("alpha", "idle", 0),
        row("beta", "running", 0),
    ])));

    play.route_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(
        matches!(play.took_host_cmd(), Some(crate::tui::HostCmd::Archive(id)) if id == "alpha"),
        "an idle session is archived on the first press"
    );

    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    play.route_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(
        play.took_host_cmd().is_none(),
        "a running session is held at the confirm"
    );
    let screen = play.render();
    assert!(screen.contains("archive it anyway?"), "{screen}");

    // Any key that is not the confirm backs out, and nothing is sent.
    play.route_key(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(play.took_host_cmd().is_none());

    play.route_key(KeyCode::Char('a'), KeyModifiers::NONE);
    play.route_key(KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(
        matches!(play.took_host_cmd(), Some(crate::tui::HostCmd::Archive(id)) if id == "beta"),
        "the confirmed archive goes through"
    );
}

/// `⏎` on another session ends the loop with a destination; the previous
/// session's stream is closed by the entry point re-attaching (docs/tui.md).
#[test]
fn enter_on_another_session_leaves_with_a_destination() {
    let mut play = Screenplay::new(90, 24);
    play.state.session_identity = Some(crate::tui::SessionIdentityView {
        id: "alpha".into(),
        title: None,
        model: "test/m".into(),
        cwd: "/work".into(),
        roles: None,
        successor: None,
        root: String::new(),
    });
    play.state.open_sessions();
    let _ = play.took_host_cmd();
    play.event(crate::tui::TuiEvent::Sessions(Ok(vec![
        row("alpha", "idle", 0),
        row("beta", "idle", 0),
    ])));

    // The attached session is a no-op that closes the overlay: re-attaching
    // where we already are would only cost the transcript its scrollback.
    assert!(!play.key_quits(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(play.state.switch_to, None);

    play.state.open_sessions();
    let _ = play.took_host_cmd();
    play.event(crate::tui::TuiEvent::Sessions(Ok(vec![
        row("alpha", "idle", 0),
        row("beta", "idle", 0),
    ])));
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    assert!(play.key_quits(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        play.state.switch_to,
        Some(Exit::Switch {
            id: "beta".into(),
            cwd: Some("/work".into()),
        })
    );
}

/// The cursor follows the id it was on, not the position: a row leaving must not
/// silently re-aim `e` at whatever slid into its place.
#[test]
fn the_cursor_follows_the_row_it_was_on() {
    let mut play = Screenplay::new(90, 24);
    play.state.open_sessions();
    let _ = play.took_host_cmd();
    play.event(crate::tui::TuiEvent::Sessions(Ok(vec![
        row("alpha", "idle", 0),
        row("beta", "idle", 0),
        row("gamma", "idle", 0),
    ])));
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    // `alpha` goes; `gamma` is now at index 1 and the cursor must be on it.
    play.event(crate::tui::TuiEvent::Sessions(Ok(vec![
        row("beta", "idle", 0),
        row("gamma", "idle", 0),
    ])));
    play.route_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(
        matches!(play.took_host_cmd(), Some(crate::tui::HostCmd::Archive(id)) if id == "gamma"),
    );
}

/// The overlay as it paints (docs/tui.md): a row per session with the status and
/// process count the listing carries, the attached one named, and the legend.
#[test]
fn the_switcher_overlay() {
    let mut play = Screenplay::new(90, 20);
    play.state.session_identity = Some(crate::tui::SessionIdentityView {
        id: "alpha".into(),
        title: None,
        model: "test/m".into(),
        cwd: "/work".into(),
        roles: None,
        successor: None,
        root: String::new(),
    });
    play.state.open_sessions();
    let _ = play.took_host_cmd();
    play.event(crate::tui::TuiEvent::Sessions(Ok(vec![
        row("alpha", "idle", 0),
        row("beta", "running", 2),
        row("gamma", "gated", 0),
    ])));
    play.snapshot("session_switcher");
}

/// A session that handed off names its successor, and the TUI follows it there
/// by the switcher's own exit (docs/tools.md).
#[test]
fn a_named_successor_is_followed() {
    let mut play = Screenplay::new(80, 12);
    let quits = play.event_quits(crate::tui::TuiEvent::SessionIdentity(Some(
        crate::tui::SessionIdentityView {
            id: "old-leaf".into(),
            title: None,
            model: "m".into(),
            cwd: "~/p".into(),
            roles: None,
            successor: Some("new-leaf".into()),
            root: "/home/someone/p".into(),
        },
    )));
    assert!(quits, "the loop leaves for the successor");
    assert!(matches!(
        play.state.switch_to.take(),
        Some(crate::tui::Exit::Switch { id, cwd }) if id == "new-leaf" && cwd.as_deref() == Some("/home/someone/p")
    ));
}
