use super::*;
use crate::clients::tui::harness::Screenplay;
use crate::clients::tui::topics::TopicUpdate;

/// The `starting` frame carrying `value`, read the way the bridge reads it.
fn frame(value: serde_json::Value) -> TuiEvent {
    match TopicUpdate::decode("starting", value) {
        Ok(Some(update)) => TuiEvent::Topic(update),
        other => panic!("not a starting frame: {other:?}"),
    }
}

fn starting(resume: bool, failure: Option<&str>) -> TuiEvent {
    let failure = failure.map_or(serde_json::Value::Null, |reason| {
        serde_json::json!({
            "reason": reason,
            "at": 1,
            "withdrawn": [{"submission_id": "s1", "text": "the first ask"}],
        })
    });
    frame(serde_json::json!({
        "id": "fresh",
        "cwd": "/work",
        "since": 1,
        "resume": resume,
        "stage": "preparing",
        "authorizing": null,
        "failure": failure,
    }))
}

/// While a session is prepared the status line says what preparation is doing,
/// naming a server whose authorization it waits on.
#[test]
fn the_status_line_says_what_preparation_is_doing() {
    let mut play = Screenplay::new(100, 16);
    play.event(starting(false, None));
    let screen = play.render();
    assert!(screen.contains("starting — loading the config"), "{screen}");

    play.event(frame(serde_json::json!({
        "id": "fresh", "cwd": "/work", "since": 1, "resume": false, "stage": "preparing",
        "authorizing": "exa", "failure": null,
    })));
    let screen = play.render();
    assert!(
        screen.contains("waiting for `exa` to be authorized"),
        "{screen}"
    );

    play.event(frame(serde_json::Value::Null));
    assert!(!play.render().contains("starting —"));
}

/// A failed start puts the turns it hands back in the composer, says why, and keeps
/// the view open when its stream ends; the strip's `retry` reopens the form for a
/// creation, and the session it makes is attached with the composer's text.
#[test]
fn a_failed_creation_keeps_the_view_and_retries_through_the_form() {
    let mut play = Screenplay::new(100, 16);
    play.event(starting(
        false,
        Some("could not prepare session: no such config"),
    ));
    assert_eq!(play.state.composer.text(), "the first ask");
    let screen = play.render();
    assert!(
        screen.contains("did not start: could not prepare session"),
        "{screen}"
    );
    assert!(
        !play.event_quits(TuiEvent::Disconnected),
        "the stream ending after a failure does not close the view"
    );

    play.state
        .activate_strip_verb(control_strip::StripVerb::Retry);
    assert!(matches!(play.took_host_cmd(), Some(HostCmd::NewSession)));
    assert!(play.event_quits(TuiEvent::Created(Ok("second".into()))));
    assert_eq!(
        play.state.switch_to,
        Some(Exit::SwitchWithDraft {
            id: "second".into(),
            draft: "the first ask".into(),
        })
    );
}

/// A failed resume retries by resuming the same session again, and switches back to
/// it once the host has it starting.
#[test]
fn a_failed_resume_retries_the_resume() {
    let mut play = Screenplay::new(100, 16);
    play.event(starting(true, Some("env: the hook exited 7")));
    assert_eq!(
        play.state.leading_strip_verb(),
        Some(control_strip::StripVerb::Retry)
    );
    play.state
        .activate_strip_verb(control_strip::StripVerb::Retry);
    assert!(matches!(play.took_host_cmd(), Some(HostCmd::Resume(id)) if id == "fresh"));
    assert!(play.event_quits(TuiEvent::Resumed(Ok("fresh".into()))));
    assert!(matches!(
        &play.state.switch_to,
        Some(Exit::SwitchWithDraft { id, .. }) if id == "fresh"
    ));
}
