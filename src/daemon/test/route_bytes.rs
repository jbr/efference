//! The bytes a session's routes answer with (docs/control-api.md), one pin per body.
//! The clients parse these, so a change to them is a change to the control channel and
//! fails here first, as the state topics' bytes do (`topic_bytes.rs`).
//!
//! The comparison is of whole strings. A body the host builds from values has its keys
//! sorted at every depth, as `serde_json::Value` sorts them; a body it serializes from a
//! type keeps the type's field order. Each pin is of the route's answer, so it holds
//! whichever way the body is built.
//!
//! The host's routes are pinned in `host/bodies.rs`, `host/projects.rs`, `host/preview.rs`
//! and `host/worktree.rs`, the sign-in and enrollment routes in `tcp/routes/test.rs`, and
//! the directory listing in `files/test/listing.rs`. The push subscription's two words are
//! pinned with the other status words (`protocol/answers/test.rs`): its routes write a
//! store that belongs to the machine, which a test does not touch.

use super::topic_bytes::queue_two;
use super::*;

/// A turn's acceptance names where it went, and a recall answers the turns it popped,
/// attachments first because the keys sort.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_and_its_recall_keep_their_bytes() {
    let (shared, _turns_rx) = Session::in_memory();
    let app = test_server(&shared).await;

    let accepted = app
        .post("/v1/turns")
        .with_json_body(&serde_json::json!({ "text": "first", "submission_id": "sub-1" }))
        .await;
    accepted.assert_status(202);
    assert_eq!(
        accepted.body(),
        r#"{"delivery":"now","status":"accepted","submission_id":"sub-1"}"#
    );
    let queued = app
        .post("/v1/turns")
        .with_json_body(&serde_json::json!({ "text": "second", "submission_id": "sub-2" }))
        .await;
    queued.assert_status(202);
    assert_eq!(
        queued.body(),
        r#"{"delivery":"queued","status":"accepted","submission_id":"sub-2"}"#
    );

    let (shared, _turns_rx) = Session::in_memory();
    queue_two(&shared);
    let app = test_server(&shared).await;
    let recalled = app.delete("/v1/turns/sub-2").await;
    recalled.assert_status(200);
    assert_eq!(
        recalled.body(),
        concat!(
            r#"{"withdrawn":[{"attachments":[{"kind":{"Text":{"bytes":12,"first_line":"hello there","lines":1,"shape":"Prose"}},"#,
            r#""name":"notes.txt","path":"/work/.efference/a/attachments/notes.txt"}],"submission_id":"sub-2","text":"second"}]}"#
        )
    );
    let nothing = app.delete("/v1/turns/sub-2").await;
    nothing.assert_status(200);
    assert_eq!(nothing.body(), r#"{"withdrawn":[]}"#);
    let rest = app.delete("/v1/turns/sub-1").await;
    assert_eq!(
        rest.body(),
        r#"{"withdrawn":[{"submission_id":"sub-1","text":"first"}]}"#
    );
}

/// The routes that answer with a word and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_routes_that_answer_with_a_word_keep_their_bytes() {
    // A held call: decided, and escalated.
    let (shared, _turns_rx) = Session::in_memory();
    let app = test_server(&shared).await;
    let call = bash_call("rm -rf build");
    let _gate = shared.register_pending(&call, Default::default());
    let escalated = app
        .post("/v1/approvals/toolu_x/escalate")
        .with_json_body(&serde_json::json!({ "source": "web", "note": "over to you" }))
        .await;
    escalated.assert_status(200);
    assert_eq!(escalated.body(), r#"{"status":"escalated"}"#);
    let resolved = app
        .put("/v1/approvals/toolu_x")
        .with_json_body(&serde_json::json!({ "decision": "approve" }))
        .await;
    resolved.assert_status(200);
    assert_eq!(resolved.body(), r#"{"status":"resolved"}"#);

    // The loop's verbs.
    let interrupted = app.post("/v1/loop/interrupt").await;
    interrupted.assert_status(200);
    assert_eq!(interrupted.body(), r#"{"status":"interrupted"}"#);

    let (shared, _turns_rx) = Session::in_memory();
    shared.append(1, user("hello")).unwrap();
    let app = test_server(&shared).await;
    let continuing = app.post("/v1/loop/continue").await;
    continuing.assert_status(202);
    assert_eq!(continuing.body(), r#"{"status":"continuing"}"#);

    let (shared, _turns_rx) = Session::in_memory();
    append_compactable_history(&shared);
    let app = test_server(&shared).await;
    let compacting = app
        .post("/v1/loop/compact")
        .with_json_body(&serde_json::json!({ "warm": true }))
        .await;
    compacting.assert_status(202);
    assert_eq!(compacting.body(), r#"{"status":"compacting"}"#);

    // A running process, killed.
    let (shared, _turns_rx) = Session::in_memory();
    shared
        .spawn_background("sleep 30".into(), None)
        .expect("spawn");
    let app = test_server(&shared).await;
    let killed = app.delete("/v1/processes/1").await;
    killed.assert_status(200);
    assert_eq!(killed.body(), r#"{"status":"killed"}"#);
}

/// A process's retained output, read after it has exited: the capture outlives the
/// process, and the answer names the cursor to resume from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_output_read_keeps_its_bytes() {
    let (shared, _turns_rx) = Session::in_memory();
    shared
        .spawn_background("printf 'seeded\\n'".into(), None)
        .expect("spawn");
    shared
        .poll_until("the output is captured and the process has exited", || {
            shared
                .process_output_read(1, None)
                .is_some_and(|read| read.content == "seeded\n" && !read.running)
        })
        .await;
    let app = test_server(&shared).await;

    let read = app.get("/v1/processes/1/output").await;
    read.assert_status(200);
    assert_eq!(
        read.body(),
        r#"{"cursor":7,"id":1,"lost":0,"output":"seeded\n","running":false}"#
    );
    let after = app.get("/v1/processes/1/output?since=7").await;
    assert_eq!(
        after.body(),
        r#"{"cursor":7,"id":1,"lost":0,"output":"","running":false}"#
    );
}

/// A notification answers with the seq of the entry it arrived as, and with nothing for a
/// slot, which has no entry yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filed_notification_keeps_its_bytes() {
    let (shared, _turns_rx) = Session::in_memory();
    let app = test_server(&shared).await;
    let slotted = app
        .post("/v1/notifications")
        .with_json_body(&serde_json::json!({ "source": "watch", "body": "running", "slot": "s" }))
        .await;
    slotted.assert_status(202);
    assert_eq!(slotted.body(), "{}");
    let arrived = app
        .post("/v1/notifications")
        .with_json_body(&serde_json::json!({ "source": "reviewer", "body": "a nit" }))
        .await;
    arrived.assert_status(202);
    assert_eq!(arrived.body(), r#"{"seq":0}"#);
}

/// A proposal resolved either way answers with the state it reached.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resolved_proposal_keeps_its_bytes() {
    let (shared, _turns_rx) = Session::in_memory();
    let app = test_server(&shared).await;
    for (state, command) in [("committed", "npm"), ("cancelled", "cargo")] {
        let seq = shared
            .append(
                0,
                proposed_edit(crate::log::ConfigChange::Command {
                    command: command.into(),
                }),
            )
            .unwrap();
        let resolved = app
            .patch(format!("/v1/proposals/{seq}").as_str())
            .with_json_body(&serde_json::json!({ "state": state }))
            .await;
        resolved.assert_status(200);
        assert_eq!(resolved.body(), format!(r#"{{"state":"{state}"}}"#));
    }
}

/// A tool call addressed by its id answers with the call and the entry carrying it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_located_tool_call_keeps_its_bytes() {
    let (shared, _turns_rx) = Session::in_memory();
    shared.append(1, user("run the tests")).unwrap();
    shared
        .append(
            1,
            Payload::assistant(crate::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![ToolCall::new(
                    crate::names::tools::BASH,
                    "toolu_9",
                    serde_json::json!({ "command": "cargo test" }),
                )],
            )),
        )
        .unwrap();
    let app = test_server(&shared).await;

    let located = app.get("/v1/tool_calls/toolu_9").await;
    located.assert_status(200);
    assert_eq!(
        located.body(),
        r#"{"call":{"id":"toolu_9","input":{"command":"cargo test"},"name":"bash"},"seq":1}"#
    );
}
