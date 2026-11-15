//! Admission-time grants (docs/permissions.md): what a turn's filesystem references add when it
//! is admitted, and what the session says when a grant could not be written or logged.

use super::*;
use crate::platform::testing::temp_dir;
use crate::record::log::{
    RawResult, ReferenceAccess, ReferenceDefinition, ReferenceTargetKind, UserSegment, WireState,
};
use crate::session::SubmittedTurn;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const READ: ReferenceAccess = ReferenceAccess {
    read: true,
    write: false,
};

/// A config file with content, and a session whose perimeter it is. The session is rooted at the
/// test process's working directory, as every `in_memory` fixture is, so a reference inside the
/// crate judges to a project-relative key (`docs`, `src`) and the assertions can name it.
fn session_with(config: &str) -> (Arc<Session>, PathBuf, tempfile::TempDir) {
    let scratch = temp_dir("admission-grants");
    let path = scratch.path().join("config.toml");
    std::fs::write(&path, config).unwrap();
    let (session, _rx) = Session::in_memory_with_role(LoadedConfig {
        source: ConfigSource::Path(path.clone()),
        ..LoadedConfig::default()
    });
    (session, path, scratch)
}

fn root() -> PathBuf {
    std::env::current_dir().expect("a working directory")
}

/// A document whose prose is followed by one occurrence per reference, through the constructor
/// the submission gate uses.
fn document(references: &[(&str, &Path, ReferenceTargetKind, ReferenceAccess)]) -> UserContent {
    let mut content = UserContent::text("please look at these");
    for (n, (original, path, kind, access)) in references.iter().enumerate() {
        let id = format!("r{}", n + 1);
        content.content.push(UserSegment::Reference(id.clone()));
        content.references.insert(
            id,
            ReferenceDefinition::filesystem(
                format!("reference {n}"),
                *original,
                path.to_string_lossy(),
                *kind,
                *access,
            ),
        );
    }
    content
}

/// Admit a turn the way the loop does at its two user-turn sites.
fn admit(session: &Session, document: UserContent, submission_id: &str) -> Seq {
    let turn = SubmittedTurn::structured(document, submission_id);
    session
        .append_submission(1, turn.payload(), submission_id.to_string(), None)
        .expect("the message is admitted")
}

fn config_edits(session: &Session) -> Vec<ConfigEdit> {
    session.with_entries(|entries| {
        entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                Payload::ConfigEdit(edit) => Some(edit.clone()),
                _ => None,
            })
            .collect()
    })
}

fn notice_body(session: &Session, slot: &str) -> Option<String> {
    session
        .pending_notifications()
        .into_iter()
        .find(|notification| notification.slot.as_deref() == Some(slot))
        .map(|notification| notification.body)
}

/// A referenced directory nothing covers becomes a read grant, at the project-relative key, in
/// the effective policy, the config file and the log — and only when the turn is admitted.
#[test]
fn admission_grants_a_referenced_directory() {
    let (session, config, _scratch) = session_with("# the perimeter\n");
    let docs = root().join("docs");

    admit(
        &session,
        document(&[("docs", &docs, ReferenceTargetKind::Directory, READ)]),
        "sub-grant",
    );

    let paths = session.effective_config().paths.clone();
    assert_eq!(
        paths.get("docs"),
        Some(&crate::config::Perms {
            read: true,
            write: false
        }),
        "the grant is in the effective policy: {paths:?}"
    );
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(
        text.contains("docs = { read = true }"),
        "the grant is in the config file: {text}"
    );
    let edits = config_edits(&session);
    assert_eq!(edits.len(), 1, "one committed edit records it: {edits:?}");
    assert_eq!(edits[0].state, EditState::Committed);
    assert!(
        edits[0].note.contains("sub-grant") || edits[0].note.contains("reference grant"),
        "the edit names the turn: {}",
        edits[0].note
    );
    // The session-visible notice the edit earns is kept, at the path's slot.
    assert!(
        notice_body(&session, "config:path:docs").is_some(),
        "the grant's notice is raised: {:?}",
        session.pending_notifications()
    );
}

/// A plain turn takes no policy work at all: it appends while another holder has the policy
/// section, which a turn that took the section for itself could not do.
#[test]
fn a_plain_turn_appends_without_the_policy_section() {
    let (session, _config, _scratch) = session_with("");
    let _section = session.policy_section();

    let (tx, rx) = std::sync::mpsc::channel();
    let worker = {
        let session = Arc::clone(&session);
        std::thread::spawn(move || {
            let turn = SubmittedTurn::new("hello", "sub-plain");
            let seq = session.append_submission(1, turn.payload(), "sub-plain".into(), None);
            tx.send(seq.is_ok()).unwrap();
        })
    };
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(appended) => assert!(appended, "the plain turn appends"),
        Err(_) => panic!("a plain turn waited for the policy section"),
    }
    worker.join().unwrap();
    assert!(config_edits(&session).is_empty());
}

/// An ask reply carries its references in the result's document; admitting that result grants
/// them, which is where `agent::batch::capture` lands a structured answer.
#[test]
fn an_ask_answer_grants_at_its_capture() {
    let (session, _config, _scratch) = session_with("");
    let docs = root().join("docs");
    let raw = RawResult::text("see docs").with_document(Some(document(&[(
        "docs",
        &docs,
        ReferenceTargetKind::Directory,
        READ,
    )])));

    session
        .append_submission(
            1,
            Payload::tool_result("toolu_1", raw, WireState::Raw),
            "sub-answer".into(),
            None,
        )
        .unwrap();

    assert!(
        session.effective_config().paths.contains_key("docs"),
        "the answer's reference granted at capture"
    );
}

/// A reference the policy in force already covers adds nothing: the admission reconciles the
/// file first, then finds no change to write.
#[test]
fn a_covered_reference_adds_nothing() {
    let (session, config, _scratch) = session_with("[paths]\n\"docs\" = { read = true }\n");
    let before = std::fs::read_to_string(&config).unwrap();
    let docs = root().join("docs");

    admit(
        &session,
        document(&[("docs", &docs, ReferenceTargetKind::Directory, READ)]),
        "sub-covered",
    );

    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        before,
        "the file is untouched"
    );
    // The reconciliation of the file's own grant is the only edit; the reference added none.
    let edits = config_edits(&session);
    assert_eq!(edits.len(), 1, "only the reconcile edit: {edits:?}");
    assert_eq!(edits[0].note, "config file edited");
}

/// A config-file write that fails adds no grant anywhere: the message stands under the perimeter
/// it had, and the file keeps its bytes.
#[test]
fn a_failed_config_write_adds_no_grant() {
    let (session, config, _scratch) = session_with("# the perimeter\n");
    let before = std::fs::read_to_string(&config).unwrap();
    *session.fail_config_write.lock().unwrap() = Some("the disk is full".into());
    let docs = root().join("docs");

    admit(
        &session,
        document(&[("docs", &docs, ReferenceTargetKind::Directory, READ)]),
        "sub-write-failed",
    );

    assert!(
        session.effective_config().paths.is_empty(),
        "nothing was folded: {:?}",
        session.effective_config().paths
    );
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        before,
        "the file keeps its bytes"
    );
    assert!(config_edits(&session).is_empty(), "no edit was appended");
    let body = notice_body(&session, "config:grant-refused").expect("one refusal notice");
    assert!(body.contains("was not added"), "{body}");
    assert!(body.contains("the disk is full"), "{body}");
}

/// A log append that fails partway through the batch leaves the file holding every grant, the
/// log and the effective policy holding the ones before the failure, and one notice that states
/// that partial outcome — while the grant that did land keeps its own notice (docs/permissions.md).
#[test]
fn a_failed_grant_append_reports_what_landed() {
    let (session, config, _scratch) = session_with("# the perimeter\n");
    let root = root();
    let docs = root.join("docs");
    let src = root.join("src");
    // The batch is emitted in key order — `docs` before `src` — so the first edit lands and the
    // second fails.
    *session.fail_committed_append_after.lock().unwrap() = Some(1);

    admit(
        &session,
        document(&[
            ("docs", &docs, ReferenceTargetKind::Directory, READ),
            ("src", &src, ReferenceTargetKind::Directory, READ),
        ]),
        "sub-partial",
    );

    let paths = session.effective_config().paths.clone();
    assert_eq!(
        paths.get("docs"),
        Some(&crate::config::Perms {
            read: true,
            write: false
        }),
        "the first grant is in force: {paths:?}"
    );
    assert!(!paths.contains_key("src"), "the second is not: {paths:?}");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(
        text.contains("docs = { read = true }"),
        "the file holds the first: {text}"
    );
    assert!(
        text.contains("src = { read = true }"),
        "and the second: {text}"
    );
    let edits = config_edits(&session);
    assert_eq!(edits.len(), 1, "one edit is in the log: {edits:?}");

    // The grant that landed keeps the notice the single-edit path would have raised.
    assert!(
        notice_body(&session, "config:path:docs").is_some(),
        "the landed grant's notice survives the later failure: {:?}",
        session.pending_notifications()
    );
    let body = notice_body(&session, "config:grant-refused").expect("one outcome notice");
    assert!(body.contains("only 1 of 2 were recorded"), "{body}");
    assert!(body.contains("The config file holds"), "{body}");
    assert!(
        !body.contains("was not added"),
        "the notice does not claim nothing was added: {body}"
    );
}

/// The append-to-fold boundary runs with the policy section held: that is what keeps a
/// competing committed writer from landing a fold between an admission's append and its fold,
/// so the fold order equals the log order (docs/permissions.md). The hook is the same one
/// `between_reads` is: one shot, at the point a competing writer would have to interleave.
#[test]
fn a_committed_fold_runs_with_the_policy_section_held() {
    let (session, _config, _scratch) = session_with("");
    let observed = Arc::new(AtomicBool::new(false));
    {
        let seen = Arc::clone(&observed);
        *session.between_append_and_fold.lock().unwrap() =
            Some(Box::new(move |shared: &Session| {
                seen.store(shared.policy_section_held(), Ordering::SeqCst);
            }));
    }
    let docs = root().join("docs");
    admit(
        &session,
        document(&[("docs", &docs, ReferenceTargetKind::Directory, READ)]),
        "sub-section",
    );
    assert!(observed.load(Ordering::SeqCst), "the fold held the section");
}

/// A turn waits in the queue until the loop claims it, and a recall takes it back out. Neither
/// step admits it, so neither grants anything — the message is not in the log, and the next
/// request will not carry it (docs/permissions.md).
#[test]
fn a_queued_or_withdrawn_turn_grants_nothing() {
    use crate::protocol::state::LoopPhase;
    let (session, config, _scratch) = session_with("# the perimeter\n");
    let before = std::fs::read_to_string(&config).unwrap();
    // Busy, so the submit queues instead of delivering to an idle loop.
    session.set_phase(LoopPhase::Requesting);
    let docs = root().join("docs");

    session
        .submit_named_turn(
            SubmittedTurn::structured(
                document(&[("docs", &docs, ReferenceTargetKind::Directory, READ)]),
                "sub-queued",
            ),
            None,
        )
        .expect("the turn queues");
    assert_eq!(session.queued_turns().len(), 1);
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        before,
        "a queued turn writes no config"
    );
    assert!(
        session.effective_config().paths.is_empty(),
        "a queued turn grants nothing: {:?}",
        session.effective_config().paths
    );
    assert!(config_edits(&session).is_empty());

    assert_eq!(session.withdraw_turns("sub-queued").len(), 1);
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        before,
        "a withdrawn turn writes no config"
    );
    assert!(
        session.effective_config().paths.is_empty(),
        "a withdrawn turn grants nothing: {:?}",
        session.effective_config().paths
    );
    assert!(
        config_edits(&session).is_empty(),
        "a withdrawn turn logs no edit"
    );
    assert!(
        notice_body(&session, "config:grant-refused").is_none(),
        "nothing to refuse: the turn never reached admission"
    );
}

/// The message is appended before anything it authorizes, so a log that refuses the message
/// leaves no grant, no file write and no notice: the references' turn was never admitted.
#[test]
fn a_message_that_cannot_be_appended_grants_nothing() {
    let (session, config, _scratch) = session_with("# the perimeter\n");
    let before = std::fs::read_to_string(&config).unwrap();
    *session.fail_log_append.lock().unwrap() = Some("the log is full".into());
    let docs = root().join("docs");

    let turn = SubmittedTurn::structured(
        document(&[("docs", &docs, ReferenceTargetKind::Directory, READ)]),
        "sub-no-append",
    );
    let result = session.append_submission(1, turn.payload(), "sub-no-append".into(), None);

    assert!(result.is_err(), "the append failed, as forced");
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        before,
        "the file keeps its bytes"
    );
    assert!(
        session.effective_config().paths.is_empty(),
        "nothing was folded: {:?}",
        session.effective_config().paths
    );
    assert!(config_edits(&session).is_empty(), "no edit was appended");
    assert!(
        notice_body(&session, "config:grant-refused").is_none(),
        "no grant notice: nothing was refused for an admitted turn"
    );
}

/// A masker that rewrites `secret` in a config-edit path: a stand-in for the `[env]` mask
/// (docs/env.md) that needs no secret value in the test process's environment.
struct RewritesSecrets;

impl crate::record::persist::PayloadMask for RewritesSecrets {
    fn mask_payload(&self, payload: &mut Payload) {
        if let Payload::ConfigEdit(edit) = payload
            && let ConfigChange::Path { path, .. } = &mut edit.change
        {
            *path = path.replace("secret", "MASKED");
        }
    }
}

/// A grant whose path the masker would rewrite is refused: the file never gets a key the log
/// would not name, and no edit is logged. The message is admitted under the permissions the
/// session already had, and the notice names the reference (docs/permissions.md).
#[test]
fn a_grant_whose_path_the_masker_rewrites_is_refused() {
    let (session, config, _scratch) = session_with("# the perimeter\n");
    let before = std::fs::read_to_string(&config).unwrap();
    session
        .log
        .lock()
        .unwrap()
        .set_masker(Arc::new(RewritesSecrets));
    let path = root().join("secret/notes.md");

    admit(
        &session,
        document(&[("secret/notes.md", &path, ReferenceTargetKind::File, READ)]),
        "sub-masked",
    );

    assert!(
        session.effective_config().paths.is_empty(),
        "nothing was folded: {:?}",
        session.effective_config().paths
    );
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        before,
        "the file is untouched"
    );
    assert!(config_edits(&session).is_empty(), "no edit was appended");
    let body = notice_body(&session, "config:grant-refused").expect("one refusal notice");
    assert!(body.contains("`secret/notes.md`"), "{body}");
    assert!(body.contains("`[env]` mask"), "{body}");
    assert!(body.contains("was not added"), "{body}");
}

/// Write access on `.efference` is granted live, and the log entry is what carries it across a
/// resume: boot loads the config file and then injects the session-surface deny at `.efference`
/// (`host/boot/scaffold.rs`), and the fold of the log — which runs after (`session/construct.rs`)
/// — outranks it even at equal specificity. A grant held only in the file would lose.
#[test]
fn the_efference_grant_holds_live_and_survives_a_resume_through_the_log() {
    use crate::config::Perms;
    let write = ReferenceAccess {
        read: true,
        write: true,
    };
    let granted = Perms {
        read: true,
        write: true,
    };
    let scratch = temp_dir("admission-resume");
    let config = scratch.path().join("config.toml");
    std::fs::write(&config, "# the perimeter\n").unwrap();
    let log_path = scratch.path().join("log.jsonl");
    let surface = root().join(".efference");

    let (session, _rx) = Session::with_role(
        std::env::current_dir().unwrap(),
        Log::create(&log_path, None).expect("a log"),
        LoadedConfig {
            source: ConfigSource::Path(config.clone()),
            ..LoadedConfig::default()
        },
    );
    admit(
        &session,
        document(&[(
            ".efference",
            &surface,
            ReferenceTargetKind::Directory,
            write,
        )]),
        "sub-surface",
    );
    assert_eq!(
        session.effective_config().paths.get(".efference"),
        Some(&granted),
        "the live admission grants the surface"
    );
    drop(session);

    // The resumed session: the file as boot leaves it (no `.efference` key), the injected deny,
    // and the log folded afterwards.
    std::fs::write(&config, "# the perimeter\n").unwrap();
    let mut role = LoadedConfig {
        source: ConfigSource::Path(config.clone()),
        ..LoadedConfig::default()
    };
    role.role
        .paths
        .insert(".efference".into(), Perms::default());
    let opened = Log::open(&log_path).expect("the log reopens");
    let (resumed, _rx) = Session::with_role(std::env::current_dir().unwrap(), opened.log, role);
    assert_eq!(
        resumed.effective_config().paths.get(".efference"),
        Some(&granted),
        "the log fold reinstates the grant over the equal-specificity deny"
    );
}

/// A competing committed writer (the fs-watch, reconciling an operator's hand-edit) that reaches
/// the policy section at the admission's append-to-fold boundary finds it held, and once both
/// settle the session reports the fold of its own log (docs/permissions.md). The witness is the
/// failed `try_lock` at that instant; the ordering is asserted on the settled state, not on which
/// thread reached the section first.
#[test]
fn a_competing_refresh_cannot_fold_between_an_admission_and_its_fold() {
    let (session, config, _scratch) = session_with("# the perimeter\n");
    let docs = root().join("docs");

    // The hook runs between the admission's message-append and its fold, with the policy section
    // held. It wakes a writer and returns only once that writer has *tried* the section and found
    // it held, so the writer cannot miss the boundary by starting late: the attempt is what the
    // two threads meet on, and the section cannot be released until it has happened.
    let (at_boundary, boundary) = std::sync::mpsc::channel();
    let (attempted, attempt) = std::sync::mpsc::channel();
    *session.between_append_and_fold.lock().unwrap() = Some(Box::new(move |_shared: &Session| {
        at_boundary.send(()).unwrap();
        attempt
            .recv_timeout(Duration::from_secs(10))
            .expect("the writer reached the policy section");
    }));
    let writer = {
        let session = Arc::clone(&session);
        let config = config.clone();
        std::thread::spawn(move || {
            boundary
                .recv_timeout(Duration::from_secs(10))
                .expect("the admission reached its boundary");
            // A lock attempt, not a wait: this is false only because admission holds the section
            // right now, which is the interleaving the test is about.
            let held = session.policy_section_held();
            // Release the hook before asserting, so a missing interleaving fails the test here
            // instead of stalling it on the rendezvous.
            attempted.send(()).unwrap();
            assert!(held, "the writer tried the section while admission held it");
            // Additive by hand: the grant the admission wrote, plus a second one beside it.
            std::fs::write(
                &config,
                "# the perimeter\n\n[paths]\ndocs = { read = true }\n\"src\" = { read = true }\n",
            )
            .unwrap();
            refresh_and_emit(&session);
        })
    };

    admit(
        &session,
        document(&[("docs", &docs, ReferenceTargetKind::Directory, READ)]),
        "sub-interleaved",
    );
    writer.join().unwrap();

    // Both grants are in force.
    let paths = session.effective_config().paths.clone();
    assert!(paths.contains_key("docs"), "{paths:?}");
    assert!(paths.contains_key("src"), "{paths:?}");

    // The oracle: the session holds exactly the fold of its own log, and the admission's grant
    // edit is earlier in the log than the watch's reconciliation.
    let entries = session.with_entries(<[_]>::to_vec);
    let folded =
        crate::config::policy::effective_config(&crate::config::Config::default(), &entries);
    assert_eq!(
        session.effective_config().paths,
        folded.paths,
        "fold order equals log order"
    );
    let notes: Vec<String> = config_edits(&session)
        .into_iter()
        .map(|edit| edit.note)
        .collect();
    let grant = notes
        .iter()
        .position(|note| note.starts_with("reference grant:"))
        .expect("the admission's edit is in the log");
    let reconciled = notes
        .iter()
        .position(|note| note == "config file edited")
        .expect("the watch's edit is in the log");
    assert!(grant < reconciled, "log order: {notes:?}");
}
