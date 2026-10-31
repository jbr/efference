use super::*;
use crate::tui::harness::Screenplay;
use crossterm::event::{KeyCode, KeyModifiers};

fn row(id: &str, status: &str, processes: usize) -> ListedSession {
    ListedSession {
        id: id.into(),
        title: Some(format!("{id}'s work")),
        cwd: Some("/work".into()),
        working: None,
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
        ("asking", 0, "busy is waiting on your answer to a question"),
        ("blocked", 0, "busy is stopped mid-turn until someone acts"),
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
    assert!(matches!(play.took_host_cmd(), Some(HostCmd::List)));
    play.event(TuiEvent::Sessions(Ok(vec![
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
    play.event(TuiEvent::Sessions(Ok(vec![
        row("alpha", "idle", 0),
        row("beta", "running", 0),
    ])));

    play.route_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(
        matches!(play.took_host_cmd(), Some(HostCmd::Archive(id)) if id == "alpha"),
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
        matches!(play.took_host_cmd(), Some(HostCmd::Archive(id)) if id == "beta"),
        "the confirmed archive goes through"
    );
}

/// `⏎` on another session ends the loop with a destination; the previous
/// session's stream is closed by the entry point re-attaching (docs/tui.md).
#[test]
fn enter_on_another_session_leaves_with_a_destination() {
    let mut play = Screenplay::new(90, 24);
    play.state.session_identity = Some(SessionIdentityView {
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
    play.event(TuiEvent::Sessions(Ok(vec![
        row("alpha", "idle", 0),
        row("beta", "idle", 0),
    ])));

    // The attached session is a no-op that closes the overlay: re-attaching
    // where we already are would only cost the transcript its scrollback.
    assert!(!play.key_quits(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(play.state.switch_to, None);

    play.state.open_sessions();
    let _ = play.took_host_cmd();
    play.event(TuiEvent::Sessions(Ok(vec![
        row("alpha", "idle", 0),
        row("beta", "idle", 0),
    ])));
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    assert!(play.key_quits(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        play.state.switch_to,
        Some(Exit::Switch { id: "beta".into() })
    );
}

/// The cursor follows the id it was on, not the position: a row leaving must not
/// silently re-aim `e` at whatever slid into its place.
#[test]
fn the_cursor_follows_the_row_it_was_on() {
    let mut play = Screenplay::new(90, 24);
    play.state.open_sessions();
    let _ = play.took_host_cmd();
    play.event(TuiEvent::Sessions(Ok(vec![
        row("alpha", "idle", 0),
        row("beta", "idle", 0),
        row("gamma", "idle", 0),
    ])));
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    play.route_key(KeyCode::Down, KeyModifiers::NONE);
    // `alpha` goes; `gamma` is now at index 1 and the cursor must be on it.
    play.event(TuiEvent::Sessions(Ok(vec![
        row("beta", "idle", 0),
        row("gamma", "idle", 0),
    ])));
    play.route_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(matches!(play.took_host_cmd(), Some(HostCmd::Archive(id)) if id == "gamma"),);
}

/// The overlay as it paints (docs/tui.md): a row per session with the status and
/// process count the listing carries, the attached one named, and the legend.
#[test]
fn the_switcher_overlay() {
    let mut play = Screenplay::new(90, 20);
    play.state.session_identity = Some(SessionIdentityView {
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
    play.event(TuiEvent::Sessions(Ok(vec![
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
    let quits = play.event_quits(TuiEvent::SessionIdentity(Some(SessionIdentityView {
        id: "old-leaf".into(),
        title: None,
        model: "m".into(),
        cwd: "~/p".into(),
        roles: None,
        successor: Some("new-leaf".into()),
        root: "/home/someone/p".into(),
    })));
    assert!(quits, "the loop leaves for the successor");
    assert!(matches!(
        play.state.switch_to.take(),
        Some(Exit::Switch { id }) if id == "new-leaf"
    ));
}

fn archived_row(id: &str, title: Option<&str>, age: &str) -> resume_list::ResumeRow {
    resume_list::ResumeRow {
        id: id.into(),
        title: title.map(Into::into),
        age: Some(age.into()),
    }
}

/// A switcher with two live rows, `r` pressed, and the archived rows landed.
fn resume_page_open() -> Screenplay<'static> {
    let mut play = Screenplay::new(100, 24);
    play.state.open_sessions();
    let _ = play.took_host_cmd();
    play.event(TuiEvent::Sessions(Ok(vec![
        row("alpha", "idle", 0),
        row("beta", "idle", 0),
    ])));
    play.route_key(KeyCode::Char('r'), KeyModifiers::NONE);
    assert!(matches!(play.took_host_cmd(), Some(HostCmd::Archived)));
    play.event(TuiEvent::Archived(Ok(vec![
        archived_row(
            "quiet-heron",
            Some("tracing the flaky socket test"),
            "16m ago",
        ),
        archived_row("plain-otter", None, "2d ago"),
        archived_row("brisk-newt", Some("porting the radish parser"), "3d ago"),
    ])));
    play
}

fn type_text(play: &mut Screenplay, text: &str) {
    for c in text.chars() {
        play.route_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
}

/// `r` opens the resume page over the listing at once, says it is reading, and
/// shows the rows when they land.
#[test]
fn r_opens_the_resume_page_and_the_rows_replace_the_reading_line() {
    let mut play = Screenplay::new(100, 24);
    play.state.open_sessions();
    let _ = play.took_host_cmd();
    play.event(TuiEvent::Sessions(Ok(vec![row("alpha", "idle", 0)])));
    play.route_key(KeyCode::Char('r'), KeyModifiers::NONE);
    assert!(matches!(play.took_host_cmd(), Some(HostCmd::Archived)));
    let screen = play.render();
    assert!(
        screen.contains("reading the archived sessions…"),
        "{screen}"
    );
    assert!(
        !screen.contains("alpha"),
        "the listing is under the modal: {screen}"
    );
    play.event(TuiEvent::Archived(Ok(vec![archived_row(
        "quiet-heron",
        Some("tracing the flaky socket test"),
        "16m ago",
    )])));
    let screen = play.render();
    assert!(
        screen.contains("❯ tracing the flaky socket test"),
        "{screen}"
    );
    assert!(!screen.contains("reading"), "{screen}");
}

#[test]
fn a_failed_read_shows_the_hosts_error_as_the_note() {
    let mut play = Screenplay::new(100, 24);
    play.state.open_sessions();
    let _ = play.took_host_cmd();
    play.route_key(KeyCode::Char('r'), KeyModifiers::NONE);
    play.event(TuiEvent::Archived(Err(
        "listing archived sessions: host said no".into(),
    )));
    let screen = play.render();
    assert!(screen.contains("host said no"), "{screen}");
    assert!(!screen.contains("reading the archived"), "{screen}");
}

/// Filter, pick: the PATCH goes out, `resuming <id>…` shows meanwhile, and the
/// answer switches exactly as `⏎` on a live row does.
#[test]
fn picking_a_row_resumes_it_and_a_success_switches() {
    let mut play = resume_page_open();
    type_text(&mut play, "radish");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(play.took_host_cmd(), Some(HostCmd::Resume(id)) if id == "brisk-newt"));
    let screen = play.render();
    assert!(screen.contains("resuming brisk-newt…"), "{screen}");
    // A second Enter while it is out sends nothing.
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(play.took_host_cmd().is_none());
    assert!(play.event_quits(TuiEvent::Resumed(Ok("brisk-newt".into()))));
    assert_eq!(
        play.state.switch_to,
        Some(Exit::Switch {
            id: "brisk-newt".into()
        })
    );
}

#[test]
fn a_failed_resume_shows_the_hosts_message_and_stays_on_the_page() {
    let mut play = resume_page_open();
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(play.took_host_cmd(), Some(HostCmd::Resume(id)) if id == "quiet-heron"));
    assert!(!play.event_quits(TuiEvent::Resumed(Err(
        "resuming quiet-heron: the directory is gone".into()
    ))));
    assert_eq!(play.state.switch_to, None);
    let screen = play.render();
    assert!(screen.contains("the directory is gone"), "{screen}");
    assert!(screen.contains("quiet-heron"), "{screen}");
    // The same row can be tried again.
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(play.took_host_cmd(), Some(HostCmd::Resume(_))));
}

/// Esc clears a filter first and then closes the page back to the listing; the
/// switcher is still open under it.
#[test]
fn escape_clears_the_filter_then_returns_to_the_listing() {
    let mut play = resume_page_open();
    type_text(&mut play, "zzz");
    play.route_key(KeyCode::Esc, KeyModifiers::NONE);
    let screen = play.render();
    assert!(screen.contains("type to filter"), "{screen}");
    assert!(screen.contains("archived sessions here"), "{screen}");
    play.route_key(KeyCode::Esc, KeyModifiers::NONE);
    let screen = play.render();
    assert!(
        screen.contains("alpha") && screen.contains("beta"),
        "{screen}"
    );
    assert!(!screen.contains("archived sessions here"), "{screen}");
    assert!(matches!(
        &play.state.ui_mode,
        UiMode::Sessions(menu) if menu.resume.is_none()
    ));
}

/// An answer that arrives after the page closed is dropped.
#[test]
fn answers_after_the_page_closed_are_ignored() {
    let mut play = resume_page_open();
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    let _ = play.took_host_cmd();
    play.route_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!play.event_quits(TuiEvent::Resumed(Ok("quiet-heron".into()))));
    assert_eq!(play.state.switch_to, None);
}

#[test]
fn the_hint_line_teaches_r() {
    let mut play = Screenplay::new(100, 24);
    play.state.open_sessions();
    let _ = play.took_host_cmd();
    play.event(TuiEvent::Sessions(Ok(vec![row("alpha", "idle", 0)])));
    assert!(play.render().contains("r:resume"));
}

#[test]
fn resume_modal_snapshots() {
    let mut play = resume_page_open();
    play.snapshot("session_switcher_resume");
    type_text(&mut play, "heron");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    let _ = play.took_host_cmd();
    play.snapshot("session_switcher_resuming");
}
