use super::super::test::{PREPARING, Root, codex_registry, drop_boot, request, scratch_dirs};
use super::*;
use crate::boot::{BASE_DIR, prepare};
use crate::log::{
    AssistantOutput, ContentBlock, ExitOutcome, ProcessEvent, ProcessEventKind, ProcessExit,
    ProcessSpawn, RawResult, Title, TitleAuthor, ToolCall, WireState,
};

/// The error a resume that must fail returned, cleaning up after it if it
/// unexpectedly succeeded. Not the shared `failure`, which is typed against the
/// bare [`SessionBoot`] `prepare` still returns.
fn failure(
    result: Result<(SessionBoot<MountedSpotter>, SessionOptions), Error>,
    what: &str,
) -> Error {
    match result {
        Ok((boot, _)) => {
            drop_boot(boot);
            panic!("{what}")
        }
        Err(error) => error,
    }
}

fn resume_request<'a>(root: &Root, id: &str, registry: &'a InferenceRegistry) -> ResumeRequest<'a> {
    ResumeRequest {
        session_dir: root.session_dir(id),
        base_env: super::super::test::base_env(),
        inference: registry,
        reauth: Vec::new(),
        host: None,
    }
}

/// A session created and left on disk with `extra` appended after its preamble, as a
/// host that stopped would leave it.
async fn left_behind(root: &Root, id: &str, extra: impl FnOnce(&mut Log)) {
    let mut boot = prepare(request(root.path(), id, None)).await.unwrap();
    extra(&mut boot.log);
    drop_boot(boot);
}

fn call_assistant(call: &str) -> Payload {
    Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(
            vec![],
            vec![],
            vec![ToolCall::new(
                "bash",
                call,
                serde_json::json!({"command": "ls"}),
            )],
        ),
        upstream_failure: None,
    }
}

fn spawned(id: u64) -> Payload {
    Payload::ProcessEvent(ProcessEvent {
        id,
        kind: ProcessEventKind::Spawned(ProcessSpawn {
            command: "sleep 99".into(),
            call_id: None,
            pid: None,
            at: Timestamp::now(),
        }),
    })
}

/// The bytes of every file in a session dir, to compare before and after a refusal.
fn dir_contents(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap_or_default(),
            )
        })
        .collect();
    files.sort();
    files
}

/// A log ending after a finished trajectory reopens under its own id: the entries
/// stand, the `Resumed` entry follows them in the last turn, and the state the log
/// carries is what the session continues from.
#[tokio::test]
async fn a_clean_log_resumes_with_its_state() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("resume-clean");
    left_behind(&root, "clean", |log| {
        log.append(1, Payload::User(vec![ContentBlock::Text("hi".into())]))
            .unwrap();
        log.append(1, call_assistant("call_1")).unwrap();
        log.append(
            1,
            Payload::tool_result("call_1", RawResult::text("ok"), WireState::Raw),
        )
        .unwrap();
        log.append(1, spawned(2)).unwrap();
        log.append(1, spawned(3)).unwrap();
        log.append(
            1,
            Payload::ProcessEvent(ProcessEvent {
                id: 3,
                kind: ProcessEventKind::Exited(ProcessExit {
                    outcome: ExitOutcome::Code(0),
                    duration_ms: 1,
                    at: Timestamp::now(),
                }),
            }),
        )
        .unwrap();
        log.append(
            3,
            Payload::Title(Title {
                text: "Listing things".into(),
                author: TitleAuthor::Operator,
            }),
        )
        .unwrap();
    })
    .await;
    let dir = root.session_dir("clean");
    let config_before = std::fs::read(dir.join(CONFIG_NAME)).unwrap();
    let preamble_before =
        frozen_preamble(&crate::persist::load(dir.join(LOG_NAME)).unwrap()).unwrap();

    let registry = codex_registry(root.path(), "clean");
    let (boot, options) = resume(resume_request(&root, "clean", &registry))
        .await
        .expect("a clean log resumes");

    // The selections come back with the boot rather than being supplied to it: a
    // resumed session has no options to override them with, and one that later
    // hands its work off has to hand over the model it is actually running.
    assert_eq!(
        options.model.as_deref(),
        Some("gpt-5.6"),
        "the model the header recorded — the native name it was created with, \
         not the connection-qualified one a fresh resolution would produce"
    );

    let entries = boot.log.entries();
    let last = entries.last().unwrap();
    let Payload::Resumed(resumed) = &last.payload else {
        panic!("the Resumed entry is last: {:?}", last.payload);
    };
    assert_eq!(resumed.lost_processes, vec!["p#2".to_string()]);
    assert!(resumed.dropped_calls.is_empty());
    assert_eq!(last.turn, 3, "filed under the log's last turn");
    assert_eq!(entries.len() as u64, last.seq + 1);

    // The restored state, read the way the runtime reads it.
    assert_eq!(crate::log::latest_title(entries), Some("Listing things"));
    let (session, _turns) = crate::daemon::Session::full(crate::daemon::SessionParts::new(
        boot.paths.cwd().to_path_buf(),
        boot.log,
    ));
    assert_eq!(
        session.latest_turn(),
        3,
        "the turn counter continues from here"
    );

    assert_eq!(std::fs::read(dir.join(CONFIG_NAME)).unwrap(), config_before);
    let reread = crate::persist::load(dir.join(LOG_NAME)).unwrap();
    assert_eq!(frozen_preamble(&reread).unwrap(), preamble_before);
}

/// A log ending mid-batch is refused with the reason, and refusing writes nothing:
/// the log, the config, and the directory are as found, and no scratch was made.
#[tokio::test]
async fn a_mid_batch_log_is_refused_untouched() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("resume-mid");
    left_behind(&root, "mid", |log| {
        log.append(1, Payload::User(vec![ContentBlock::Text("go".into())]))
            .unwrap();
        log.append(1, call_assistant("call_9")).unwrap();
    })
    .await;
    let dir = root.session_dir("mid");
    let before = dir_contents(&dir);
    let scratch_before = scratch_dirs();

    let registry = codex_registry(root.path(), "mid");
    let error = failure(
        resume(resume_request(&root, "mid", &registry)).await,
        "an unanswered call must refuse the resume",
    );
    let text = error.to_string();
    assert!(
        text.contains("mid-batch") && text.contains("call_9"),
        "{text}"
    );
    assert_eq!(dir_contents(&dir), before);
    assert_eq!(scratch_dirs(), scratch_before);
}

/// A result captured but not yet decided is the other unclean end.
#[tokio::test]
async fn an_undecided_result_is_refused() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("resume-pending");
    left_behind(&root, "pending", |log| {
        log.append(1, call_assistant("call_2")).unwrap();
        log.append(
            1,
            Payload::tool_result("call_2", RawResult::text("out"), WireState::Pending),
        )
        .unwrap();
    })
    .await;
    let registry = codex_registry(root.path(), "pending");
    let error = failure(
        resume(resume_request(&root, "pending", &registry)).await,
        "an undecided result must refuse the resume",
    );
    assert!(error.to_string().contains("never decided"), "{error}");
}

/// A failure after the log is read (here, a connection the registry no longer has)
/// leaves the existing session directory as found.
#[tokio::test]
async fn a_failed_resume_leaves_the_session_dir_alone() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("resume-fail");
    left_behind(&root, "fail", |_| {}).await;
    let dir = root.session_dir("fail");
    let before = dir_contents(&dir);
    let scratch_before = scratch_dirs();

    let error = failure(
        resume(resume_request(&root, "fail", &InferenceRegistry::empty())).await,
        "an unresolvable connection must fail the resume",
    );
    assert!(error.to_string().contains("oai"), "{error}");
    assert_eq!(dir_contents(&dir), before);
    assert_eq!(scratch_dirs(), scratch_before);
    assert!(dir.starts_with(root.path().join(BASE_DIR)));

    // The log is still resumable once the failure is gone.
    let registry = codex_registry(root.path(), "fail");
    let (boot, _) = resume(resume_request(&root, "fail", &registry))
        .await
        .expect("the same directory resumes afterwards");
    drop_boot(boot);
}

/// A second resume of a log a live session holds is refused by the writer lock.
#[tokio::test]
async fn a_held_log_is_refused() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("resume-held");
    left_behind(&root, "held", |_| {}).await;
    let registry = codex_registry(root.path(), "held");
    let (first, _) = resume(resume_request(&root, "held", &registry))
        .await
        .unwrap();
    let before = dir_contents(&root.session_dir("held"));
    let error = failure(
        resume(resume_request(&root, "held", &registry)).await,
        "a held log must be refused",
    );
    assert!(error.to_string().contains("another writer"), "{error}");
    assert_eq!(dir_contents(&root.session_dir("held")), before);
    drop_boot(first);
}
