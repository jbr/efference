use super::*;
use crate::tui::harness::Screenplay;
use crate::tui::{Exit, TuiEvent};

fn log() -> PathBuf {
    PathBuf::from("/work/app/.efference/hold-probe")
}

/// The stream ending closes the view with `Exit::Closed`, so the entry point can say why
/// once the terminal is restored.
#[test]
fn the_stream_ending_exits_as_closed() {
    let mut play = Screenplay::new(100, 16);
    assert!(play.event_quits(TuiEvent::Disconnected));
    assert_eq!(play.state.switch_to, Some(Exit::Closed));
}

#[test]
fn an_archive_names_the_session_the_archiver_and_the_log() {
    let by = Closure::Archived {
        id: "hold-probe".into(),
        by: Some("mcp[claude-code]".into()),
        log: log(),
    };
    assert_eq!(
        by.line(),
        "efference: hold-probe was archived by mcp[claude-code]; \
         its log stays at /work/app/.efference/hold-probe/"
    );
    let anonymous = Closure::Archived {
        id: "hold-probe".into(),
        by: None,
        log: log(),
    };
    assert_eq!(
        anonymous.line(),
        "efference: hold-probe was archived; its log stays at /work/app/.efference/hold-probe/"
    );
}

#[test]
fn the_other_answers_each_have_a_line() {
    let gone = Closure::HostGone {
        socket: Some(PathBuf::from("/home/me/.efference/host.sock")),
    };
    assert_eq!(
        gone.line(),
        "efference: lost the connection to the host at /home/me/.efference/host.sock"
    );
    let stopped = Closure::Stopped {
        id: "hold-probe".into(),
        log: log(),
    };
    assert_eq!(
        stopped.line(),
        "efference: hold-probe stopped when the host did; \
         its log stays at /work/app/.efference/hold-probe/"
    );
    let hosted = Closure::StillHosted {
        id: "hold-probe".into(),
        status: "idle".into(),
    };
    assert_eq!(
        hosted.line(),
        "efference: the stream for hold-probe ended while the host still lists it as idle"
    );
    let unknown = Closure::Unknown {
        id: "hold-probe".into(),
    };
    assert_eq!(
        unknown.line(),
        "efference: hold-probe is no longer on the host"
    );
}

/// A host whose socket is gone does not answer, and the line names the socket the
/// client dialled.
#[tokio::test]
async fn a_socket_nobody_listens_on_is_the_host_gone() {
    let dir = std::env::temp_dir().join(format!("efference-closed-{}", std::process::id()));
    let socket = dir.join("host.sock");
    let client = Client::at(&socket, "/sessions/hold-probe");
    let closure = ask_within(&client, Duration::ZERO).await;
    assert_eq!(
        closure,
        Closure::HostGone {
            socket: Some(socket.clone())
        }
    );
    assert_eq!(
        closure.line(),
        format!(
            "efference: lost the connection to the host at {}",
            socket.display()
        )
    );
}
