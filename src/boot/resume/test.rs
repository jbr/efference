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
    assert_eq!(resumed.resumes_after, None);
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

/// A log ending mid-batch resumes from just before the unfinished completion: the
/// `Resumed` entry names that point, the skipped entries stay on disk, and nothing but
/// the appended entry changes.
#[tokio::test]
async fn a_mid_batch_log_rolls_back() {
    let _serial = PREPARING.lock().await;
    let root = Root::new("resume-mid");
    left_behind(&root, "mid", |log| {
        log.append(1, Payload::User(vec![ContentBlock::Text("go".into())]))
            .unwrap();
        log.append(1, call_assistant("call_9")).unwrap();
        log.append(1, spawned(4)).unwrap();
    })
    .await;
    let dir = root.session_dir("mid");
    let before = std::fs::read(dir.join(LOG_NAME)).unwrap();
    let other_files: Vec<_> = dir_contents(&dir)
        .into_iter()
        .filter(|(name, _)| name != LOG_NAME)
        .collect();
    let user_seq = crate::persist::load(dir.join(LOG_NAME))
        .unwrap()
        .iter()
        .position(|e| matches!(e.payload, Payload::User(_)))
        .unwrap() as u64;

    let registry = codex_registry(root.path(), "mid");
    let (boot, _) = resume(resume_request(&root, "mid", &registry))
        .await
        .expect("an unfinished batch rolls back");
    let entries = boot.log.entries();
    let Payload::Resumed(resumed) = &entries.last().unwrap().payload else {
        panic!("the Resumed entry is last");
    };
    assert_eq!(resumed.resumes_after, Some(user_seq));
    assert_eq!(
        resumed.lost_processes,
        vec!["p#4".to_string()],
        "a spawn in the skipped range is still a lost process"
    );
    assert_eq!(entries.last().unwrap().turn, 1);
    let notice = crate::project::render_resumed(resumed, entries.last().unwrap().seq);
    assert!(notice.contains("2 entries after seq"), "{notice}");
    let wire = serde_json::to_string(&crate::project::project(entries)).unwrap();
    assert!(!wire.contains("call_9"), "{wire}");

    // The session's turn and process numbering stay above what is on disk.
    let (session, _turns) = crate::daemon::Session::full(crate::daemon::SessionParts::new(
        boot.paths.cwd().to_path_buf(),
        boot.log,
    ));
    assert_eq!(session.latest_turn(), 1);
    drop(session);

    let after = std::fs::read(dir.join(LOG_NAME)).unwrap();
    assert!(after.starts_with(&before), "the log only grew");
    let other_after: Vec<_> = dir_contents(&dir)
        .into_iter()
        .filter(|(name, _)| name != LOG_NAME)
        .collect();
    assert_eq!(other_after, other_files);
}

/// A result captured but not yet decided is the other unclean end, and resuming
/// again after a rollback does not roll back a second time.
#[tokio::test]
async fn an_undecided_result_rolls_back_once() {
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
    let (boot, _) = resume(resume_request(&root, "pending", &registry))
        .await
        .expect("an undecided result rolls back");
    let Payload::Resumed(first) = &boot.log.entries().last().unwrap().payload else {
        panic!("the Resumed entry is last");
    };
    let first_after = first.resumes_after;
    assert!(first_after.is_some());
    drop_boot(boot);

    let (boot, _) = resume(resume_request(&root, "pending", &registry))
        .await
        .expect("the rolled-back log resumes");
    let Payload::Resumed(second) = &boot.log.entries().last().unwrap().payload else {
        panic!("the Resumed entry is last");
    };
    assert_eq!(
        second.resumes_after, None,
        "the skipped completion is not live"
    );
    drop_boot(boot);
}

/// The rollback point alone: an earlier unclean completion keeps the walk going, and a
/// clean log rolls back nowhere.
#[test]
fn the_rollback_point_walks_back_to_a_clean_end() {
    use crate::log::LogEntry;
    fn at(seq: u64, payload: Payload) -> LogEntry {
        LogEntry {
            seq,
            turn: 1,
            at: Timestamp(seq),
            submission_id: None,
            source: None,
            payload,
        }
    }
    let user = |seq| at(seq, Payload::User(vec![ContentBlock::Text("hi".into())]));
    let clean = vec![
        user(0),
        at(1, call_assistant("a")),
        at(
            2,
            Payload::tool_result("a", RawResult::text("ok"), WireState::Raw),
        ),
    ];
    assert_eq!(rollback_point(&clean), Ok(None));

    let mut mid = clean.clone();
    mid.push(at(3, call_assistant("b")));
    assert_eq!(rollback_point(&mid), Ok(Some(2)));

    // The earlier completion is itself unfinished: the walk continues past it.
    let mut nested = vec![user(0), at(1, call_assistant("a"))];
    nested.push(at(2, call_assistant("b")));
    assert_eq!(rollback_point(&nested), Ok(Some(0)));

    // Nothing before the unfinished completion to continue from.
    assert!(rollback_point(&[at(0, call_assistant("a"))]).is_err());
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
