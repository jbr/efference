use super::*;
use efference::record::log::{
    AssistantOutput, ConfigChange, ConfigEdit, EditState, Payload, Spot, Stance, ToolCall,
};
use efference::record::persist::Log;
use serde_json::json;

/// A project holding one recorded session, `held`: asked to clean the build, it proposed
/// `cargo clean`, which the spotter gated with a probe, and only then was it granted
/// `/later` and told something more. The session's config file holds that later grant,
/// as an approved grant is written back to it.
pub(crate) fn recorded_project() -> (tempfile::TempDir, PathBuf) {
    let project = tempfile::TempDir::with_prefix("efference-spotter-request-").unwrap();
    let session_dir = project.path().join(SESSION_BASE).join("held");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join(efference::host::boot::CONFIG_NAME),
        "scope = \"tidy the build\"\n\n[paths]\n\".\" = true\n\"/later\" = true\n",
    )
    .unwrap();
    let log_path = session_dir.join(LOG_NAME);
    let mut log = Log::create(&log_path, None).unwrap();
    log.append(1, Payload::user("clean the build")).unwrap();
    log.append(
        1,
        Payload::assistant(AssistantOutput::from_legacy_messages(
            vec![],
            vec![],
            vec![ToolCall::new(
                "bash",
                "c1",
                json!({ "command": "cargo clean" }),
            )],
        )),
    )
    .unwrap();
    log.append(
        1,
        Payload::Spot(
            Spot::new("c1", Stance::Watch, "a deletion").with_probed_prefix(Some("cargo".into())),
        ),
    )
    .unwrap();
    log.append(
        1,
        Payload::ConfigEdit(ConfigEdit::new(
            EditState::Committed,
            ConfigChange::Path {
                path: "/later".into(),
                read: true,
                write: true,
            },
        )),
    )
    .unwrap();
    log.append(2, Payload::user("and the docs")).unwrap();
    (project, log_path)
}

/// The prose view is the request the verdict at seq 2 was given: the scope block from
/// the config as it stood, the utterances said before it, and the move with its
/// promotion question, a breakpoint after each of the first two. Nothing granted or
/// said after the verdict is in it.
#[test]
fn the_prose_view_is_the_request_as_the_verdict_was_given_it() {
    let (_project, log) = recorded_project();
    let rebuilt = rebuild(&log, Some(Handle::parse("2").unwrap()), None).unwrap();
    assert_eq!(rebuilt.session, "held");
    let circumstances = rebuilt.recorded.circumstances(None);
    let rendered = render(&rebuilt, &circumstances, RungArg::Prose, false).unwrap();

    let blocks: Vec<&str> = rendered.split(BREAKPOINT_MARKER).collect();
    let [scope, said, fresh] = blocks.as_slice() else {
        panic!("two breakpoints in order:\n{rendered}");
    };
    assert!(scope.starts_with("Mission:\ntidy the build\n"), "{scope}");
    assert!(scope.contains("— granted as `.`"), "{scope}");
    assert!(scope.contains("efference-<scratch>"), "{scope}");
    assert!(!scope.contains("/later"), "a later grant:\n{scope}");
    let id = &rebuilt.recorded.utterances.id;
    assert_eq!(
        *said,
        format!(
            "The conversation in this session, oldest first:\n\n<operator id=\"{id}\">\n\
             clean the build\n</operator id=\"{id}\">\n\n"
        )
    );
    assert!(
        fresh.starts_with(
            "---\n\nThe agent is about to make this move:\n\nA shell command:\n\n    cargo clean"
        ),
        "{fresh}"
    );
    assert!(
        fresh.contains("considered for promotion: the command prefix `cargo`"),
        "{fresh}"
    );
    assert!(
        fresh.ends_with("Call `spot` with your stance and a one-line reason.\n"),
        "{fresh}"
    );
    assert!(rebuilt.notes.iter().any(|note| note.contains("`/later`")));

    // With `--system`, the persona comes first, under its own heading.
    let whole = render(&rebuilt, &circumstances, RungArg::Prose, true).unwrap();
    let persona = efference::curation::spotter::preview_request(
        &rebuilt.role,
        &rebuilt.recorded.judged,
        &circumstances,
        &rebuilt.directories,
        None,
    )
    .0;
    assert_eq!(
        whole,
        format!("── system ──\n{persona}── user ──\n{rendered}")
    );
}

/// The call's id names the same verdict, and the decision rung's view shows its two
/// requests: one state carrying the move and what was said before the verdict, the
/// battery in each wording, and the harness's grants fact.
#[test]
fn the_call_id_names_the_verdict_and_the_jev_view_shows_its_requests() {
    let (_project, log) = recorded_project();
    let rebuilt = rebuild(&log, None, Some("c1")).unwrap();
    assert_eq!(rebuilt.recorded.spot.as_ref().map(|(seq, _)| *seq), Some(2));
    let circumstances = rebuilt.recorded.circumstances(None);
    let rendered = render(&rebuilt, &circumstances, RungArg::Jev, false).unwrap();
    assert!(rendered.starts_with("── state ──\n{"), "{rendered}");
    assert!(
        rendered.contains("\"command\": \"cargo clean\""),
        "{rendered}"
    );
    assert!(
        rendered.contains("\"conversation\": [\n    {\n      \"text\": \"clean the build\""),
        "{rendered}"
    );
    assert!(!rendered.contains("and the docs"), "{rendered}");
    let positive = rendered
        .find("── positive request: questions ──\n")
        .expect("the positive request");
    let negative = rendered
        .find("── negative request: questions ──\n")
        .expect("the negative request");
    assert!(positive < negative);
    // The probe the verdict records is asked once, in the positive request.
    assert!(rendered[positive..negative].contains("\"prefix_safe\""));
    assert!(!rendered[negative..].contains("\"prefix_safe\""));
    assert!(
        rendered.ends_with("── outside the grants ──\nno\n"),
        "{rendered}"
    );
}
