use super::super::test::{Root, codex_registry, drop_boot, minting, request};
use super::*;
use crate::boot::{BASE_DIR, prepare};
use crate::log::Telemetry;
use crate::log::{
    AssistantOutput, CallId, ContentBlock, ExitOutcome, ProcessEvent, ProcessEventKind,
    ProcessExit, ProcessSpawn, RawResult, Title, TitleAuthor, ToolCall, WireState,
};
use crate::recovery::{CallState, last_live_completion, pending_batch};

mod switch;

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
        authorization: Default::default(),
        switch: Default::default(),
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
        excluded: None,
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
        boot.paths.directories(),
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
        boot.paths.directories(),
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

/// A call that started and has no result is the crash case: the log rolls back past its
/// completion, and resuming again does not roll back a second time.
#[tokio::test]
async fn a_started_call_rolls_back_once() {
    let root = Root::new("resume-started");
    left_behind(&root, "started", |log| {
        log.append(1, call_assistant("call_2")).unwrap();
        log.append(
            1,
            Payload::Telemetry(Telemetry::CallStarted {
                call_id: "call_2".into(),
            }),
        )
        .unwrap();
    })
    .await;
    let registry = codex_registry(root.path(), "started");
    let (boot, _) = resume(resume_request(&root, "started", &registry))
        .await
        .expect("a started call rolls back");
    let Payload::Resumed(first) = &boot.log.entries().last().unwrap().payload else {
        panic!("the Resumed entry is last");
    };
    assert!(first.resumes_after.is_some());
    assert!(first.pending_calls.is_empty());
    drop_boot(boot);

    let (boot, _) = resume(resume_request(&root, "started", &registry))
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

/// A call held at an approval gate never ran, so the session resumes at it: nothing is
/// skipped, the `Resumed` entry names the pending calls, and the notice projects behind
/// the batch's results rather than after the unanswered `tool_use`.
#[tokio::test]
async fn a_gate_held_log_resumes_at_the_gate() {
    let root = Root::new("resume-gate");
    left_behind(&root, "gate", |log| {
        log.append(1, Payload::User(vec![ContentBlock::Text("go".into())]))
            .unwrap();
        log.append(1, call_assistant("call_5")).unwrap();
        log.append(1, gate_spot("call_5")).unwrap();
    })
    .await;
    let registry = codex_registry(root.path(), "gate");
    let (boot, _) = resume(resume_request(&root, "gate", &registry))
        .await
        .expect("a gate-held log resumes");
    let entries = boot.log.entries();
    let Payload::Resumed(resumed) = &entries.last().unwrap().payload else {
        panic!("the Resumed entry is last");
    };
    assert_eq!(resumed.resumes_after, None, "nothing is rolled back");
    assert_eq!(resumed.pending_calls, vec![CallId("call_5".into())]);
    let notice = crate::project::render_resumed(resumed, entries.last().unwrap().seq);
    assert!(
        notice.contains("stopped with 1 tool call pending")
            && notice.contains("operator's next action brings it back"),
        "{notice}"
    );
    // Nothing happens at load: the pending call is still without a result.
    assert!(
        entries
            .iter()
            .all(|e| !matches!(e.payload, Payload::ToolResult { .. }))
    );
    assert!(pending_batch(entries).is_some());
    drop_boot(boot);
}

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

fn user(seq: u64) -> LogEntry {
    at(seq, Payload::User(vec![ContentBlock::Text("hi".into())]))
}

fn calls_assistant(ids: &[&str]) -> Payload {
    Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(
            vec![],
            vec![],
            ids.iter()
                .map(|id| ToolCall::new("bash", *id, serde_json::json!({"command": "ls"})))
                .collect(),
        ),
        excluded: None,
    }
}

fn started(id: &str) -> Payload {
    Payload::Telemetry(Telemetry::CallStarted { call_id: id.into() })
}

fn gate_spot(id: &str) -> Payload {
    Payload::Spot(crate::log::Spot {
        call_id: id.into(),
        stance: crate::log::Stance::Gate,
        reason: "out of scope".into(),
        origin: crate::log::Origin::Model,
        model: None,
        probed_prefix: None,
        axes: None,
    })
}

fn result(id: &str, wire: WireState) -> Payload {
    Payload::tool_result(id, RawResult::text("out"), wire)
}

fn states(entries: &[LogEntry]) -> Vec<CallState> {
    last_live_completion(entries)
        .unwrap()
        .unwrap()
        .calls
        .into_iter()
        .map(|(_, state)| state)
        .collect()
}

/// Each call of the last live completion is decided, captured, never started or started,
/// and the log's markers change how a resultless call reads.
#[test]
fn calls_classify_by_what_the_log_holds() {
    // Decided (a result with a verdict, and a result that was never pending), captured
    // (pending, no verdict), never started (no result, no marker), started.
    let log = vec![
        user(0),
        at(1, calls_assistant(&["a", "b", "c", "d", "e"])),
        at(2, started("a")),
        at(3, result("a", WireState::Raw)),
        at(4, started("b")),
        at(5, result("b", WireState::Pending)),
        at(
            6,
            Payload::ResultVerdict {
                for_result: 5,
                wire: WireState::Raw,
            },
        ),
        at(7, started("c")),
        at(8, result("c", WireState::Pending)),
        at(9, started("e")),
    ];
    assert_eq!(
        states(&log),
        vec![
            CallState::Decided,
            CallState::Decided,
            CallState::Captured { result_seq: 8 },
            CallState::NeverStarted,
            CallState::Started,
        ]
    );
    // A started call anywhere rolls the whole completion back, past the user turn.
    assert_eq!(
        resume_point(&log),
        Ok(Resumption {
            resumes_after: Some(0),
            pending: vec![],
        })
    );
    assert!(pending_batch(&log).is_none());

    // The same log without the started call resumes at the batch, in call order.
    let held = &log[..9];
    assert_eq!(
        resume_point(held),
        Ok(Resumption {
            resumes_after: None,
            pending: vec![CallId("c".into()), CallId("d".into()), CallId("e".into())],
        })
    );
    let batch = pending_batch(held).expect("a pending batch");
    assert_eq!(batch.seq, 1);
}

/// A log with no `CallStarted` anywhere predates the marker: a resultless call counts as
/// never started only when a gating `Spot` names it, and as started otherwise.
#[test]
fn a_pre_marker_log_reads_a_gate_spot_as_never_started() {
    let gated = vec![
        user(0),
        at(1, calls_assistant(&["a", "b"])),
        at(2, result("a", WireState::Raw)),
        at(3, gate_spot("b")),
    ];
    assert_eq!(
        states(&gated),
        vec![CallState::Decided, CallState::NeverStarted]
    );
    assert_eq!(resume_point(&gated).unwrap().resumes_after, None);

    // No Spot: it may have run.
    let bare = vec![user(0), at(1, calls_assistant(&["a"]))];
    assert_eq!(states(&bare), vec![CallState::Started]);
    assert_eq!(resume_point(&bare).unwrap().resumes_after, Some(0));

    // A clearing Spot is not a gate.
    let mut cleared = bare.clone();
    cleared.push(at(
        2,
        Payload::Spot(crate::log::Spot {
            stance: crate::log::Stance::Clear,
            ..match gate_spot("a") {
                Payload::Spot(spot) => spot,
                _ => unreachable!(),
            }
        }),
    ));
    assert_eq!(states(&cleared), vec![CallState::Started]);

    // With a marker elsewhere in the log, the absence of one is the evidence.
    let mut marked = gated.clone();
    marked.insert(1, at(1, calls_assistant(&["z"])));
    marked.insert(2, at(2, started("z")));
    marked.insert(3, at(3, result("z", WireState::Raw)));
    for (index, entry) in marked.iter_mut().enumerate() {
        entry.seq = index as u64;
    }
    assert_eq!(
        states(&marked),
        vec![CallState::Decided, CallState::NeverStarted]
    );
}

/// The rollback walk alone: an earlier unclean completion keeps the walk going, and a
/// clean log rolls back nowhere.
#[test]
fn the_rollback_walks_back_to_a_clean_end() {
    let clean = vec![
        user(0),
        at(1, call_assistant("a")),
        at(2, result("a", WireState::Raw)),
    ];
    assert_eq!(resume_point(&clean).unwrap().resumes_after, None);

    let mut mid = clean.clone();
    mid.push(at(3, call_assistant("b")));
    assert_eq!(resume_point(&mid).unwrap().resumes_after, Some(2));

    // The earlier completion is itself unfinished: the walk continues past it.
    let nested = vec![
        user(0),
        at(1, call_assistant("a")),
        at(2, call_assistant("b")),
    ];
    assert_eq!(resume_point(&nested).unwrap().resumes_after, Some(0));

    // Nothing before the unfinished completion to continue from.
    assert!(resume_point(&[at(0, call_assistant("a"))]).is_err());
}

/// A failure after the log is read (here, a connection the registry no longer has)
/// leaves the existing session directory as found.
#[tokio::test]
async fn a_failed_resume_leaves_the_session_dir_alone() {
    let root = Root::new("resume-fail");
    left_behind(&root, "fail", |_| {}).await;
    let dir = root.session_dir("fail");
    let before = dir_contents(&dir);

    let (resumed, scratch) = minting(resume(resume_request(
        &root,
        "fail",
        &InferenceRegistry::empty(),
    )))
    .await;
    let error = failure(resumed, "an unresolvable connection must fail the resume");
    assert!(error.to_string().contains("oai"), "{error}");
    assert_eq!(dir_contents(&dir), before);
    assert!(
        scratch.iter().all(|dir| !dir.exists()),
        "no scratch it minted survives: {scratch:?}"
    );
    assert!(dir.starts_with(root.path().join(BASE_DIR)));

    // The log is still resumable once the failure is gone.
    let registry = codex_registry(root.path(), "fail");
    let (boot, _) = resume(resume_request(&root, "fail", &registry))
        .await
        .expect("the same directory resumes afterwards");
    drop_boot(boot);
}

/// A session whose own worktree archiving removed resumes with the worktree back at
/// the same path on the same branch, the frozen preamble unchanged, and the re-creation
/// logged after `Resumed`.
#[tokio::test]
async fn an_archived_worktree_is_restored_on_resume() {
    let root = Root::new("resume-worktree");
    crate::worktree::testing::repository(root.path());
    let mut req = request(root.path(), "wt-fox", None);
    req.worktree = true;
    let boot = prepare(req).await.unwrap();
    let worktree = crate::worktree::path(root.path(), "wt-fox");
    assert!(worktree.is_dir());
    drop_boot(boot);
    let dir = root.session_dir("wt-fox");
    let preamble_before =
        frozen_preamble(&crate::persist::load(dir.join(LOG_NAME)).unwrap()).unwrap();

    let env = crate::env_table::ChildEnv::default();
    assert!(matches!(
        crate::worktree::retire(root.path(), "wt-fox", &env),
        crate::worktree::Retired::Removed { .. }
    ));
    assert!(!worktree.exists());

    let registry = codex_registry(root.path(), "wt-fox");
    let (boot, _) = resume(resume_request(&root, "wt-fox", &registry))
        .await
        .expect("a removed own worktree is re-created");
    assert!(worktree.is_dir());
    assert_eq!(boot.paths.working(), worktree);
    let entries = boot.log.entries();
    assert!(matches!(
        &entries[entries.len() - 2].payload,
        Payload::Resumed(_)
    ));
    let Payload::Worktree(event) = &entries.last().unwrap().payload else {
        panic!("the restore is logged last: {:?}", entries.last());
    };
    assert_eq!(event.branch, "efference/wt-fox");
    assert!(matches!(
        event.kind,
        crate::log::WorktreeEventKind::Restored(_)
    ));
    drop_boot(boot);
    let reread = crate::persist::load(dir.join(LOG_NAME)).unwrap();
    assert_eq!(frozen_preamble(&reread).unwrap(), preamble_before);
}

/// A second resume of a log a live session holds is refused by the writer lock.
#[tokio::test]
async fn a_held_log_is_refused() {
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

/// A created session records the `tools[]` it advertises, and a resume with the same set
/// adds neither a notice line nor a second record.
#[tokio::test]
async fn an_unchanged_tool_set_adds_nothing_on_resume() {
    let root = Root::new("resume-tools-same");
    left_behind(&root, "same", |_| {}).await;
    let dir = root.session_dir("same");
    let before = crate::persist::load(dir.join(LOG_NAME)).unwrap();
    let recorded = crate::log::recorded_tools(&before).expect("boot records tools[]");
    assert!(!recorded.is_empty());

    let registry = codex_registry(root.path(), "same");
    let (boot, _) = resume(resume_request(&root, "same", &registry))
        .await
        .expect("resumes");
    let entries = boot.log.entries();
    let Payload::Resumed(resumed) = &entries.last().unwrap().payload else {
        panic!("Resumed is last: no second tools record");
    };
    assert!(resumed.tools_changed.is_empty());
    assert!(!crate::project::render_resumed(resumed, 99).contains("tools available"));
    drop_boot(boot);
}

/// One tool whose schema changed and one that is gone are both named in the notice, and
/// the array now advertised is recorded for the next resume to compare with.
#[tokio::test]
async fn a_changed_tool_set_is_named_in_the_notice() {
    let root = Root::new("resume-tools-changed");
    left_behind(&root, "changed", |log| {
        let mut tools = crate::log::recorded_tools(log.entries()).unwrap().to_vec();
        tools[0]["description"] = serde_json::json!("an older description");
        tools.push(serde_json::json!({"name": "ghost__lookup", "input_schema": {}}));
        log.append(0, Payload::Telemetry(Telemetry::Tools { tools }))
            .unwrap();
    })
    .await;
    let changed_name = {
        let entries = crate::persist::load(root.session_dir("changed").join(LOG_NAME)).unwrap();
        crate::log::recorded_tools(&entries).unwrap()[0]["name"]
            .as_str()
            .unwrap()
            .to_string()
    };

    let registry = codex_registry(root.path(), "changed");
    let (boot, _) = resume(resume_request(&root, "changed", &registry))
        .await
        .expect("resumes");
    let entries = boot.log.entries();
    let resumed = entries
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            Payload::Resumed(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert_eq!(resumed.tools_changed.removed, vec!["ghost__lookup"]);
    assert_eq!(resumed.tools_changed.changed, vec![changed_name.clone()]);
    let notice = crate::project::render_resumed(resumed, 99);
    assert!(
        notice.contains(&format!("changed: {changed_name}"))
            && notice.contains("removed: ghost.lookup"),
        "{notice}"
    );
    let now = crate::log::recorded_tools(entries).unwrap();
    assert!(now.iter().all(|t| t["name"] != "ghost__lookup"));
    drop_boot(boot);
}
