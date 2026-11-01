//! Tests for `starting.rs`: the bytes a starting entry's `GET /state` carries
//! (docs/control-api.md), pinned for each way preparation stands — under way, waiting on
//! an authorization with turns queued, failed with those turns handed back, and stopped.
//! The same pins as `daemon/test/topic_bytes.rs`, for the four topics a starting entry
//! publishes.

use super::*;

/// The instant the pinned entries started and failed at.
const AT: u64 = 1_754_300_000_000;

/// A starting entry for `work` under `/work`, its start fixed at [`AT`].
fn entry(resume: bool) -> Arc<Starting> {
    let mut entry = Starting::new(StartingParts {
        id: "amber-heron".into(),
        directories: crate::config::policy::Directories::new("/work".into(), "/work/tree".into()),
        resume,
        created_by: None,
        detached: false,
    });
    Arc::get_mut(&mut entry).expect("not yet shared").since = AT;
    entry
}

fn arrival() -> crate::record::log::attachment::Arrival {
    crate::record::log::attachment::Arrival {
        name: "notes.txt".into(),
        path: "/work/.efference/a/attachments/notes.txt".into(),
        kind: crate::record::log::attachment::Kind::Binary { bytes: 3 },
    }
}

fn queue_two(entry: &Starting) {
    entry
        .submit(SubmittedTurn::new("first", "sub-1"))
        .expect("a starting entry queues turns");
    entry
        .submit(
            SubmittedTurn::new("second", "sub-2")
                .sourced(Some("session[calm-finch]".into()))
                .with_attachments(vec![arrival()]),
        )
        .expect("a starting entry queues turns");
}

#[test]
fn a_starting_entrys_state_keeps_its_bytes() {
    assert_eq!(
        entry(false).state_json(),
        r#"{"last_seq":null,"queued":[],"starting":{"authorizing":null,"cwd":"/work","failure":null,"id":"amber-heron","resume":false,"since":1754300000000,"stage":"environment"},"status":{"condition":{"kind":"starting","since":1754300000000,"stage":"environment"},"footer":{},"id":"amber-heron"},"verbs":{"compact":{"accepted":false,"message":"session \"amber-heron\" is starting: its environment and MCP servers are being prepared, and only a turn is accepted until it has started"},"continue":{"accepted":false,"message":"session \"amber-heron\" is starting: its environment and MCP servers are being prepared, and only a turn is accepted until it has started"},"turn":{"accepted":true,"delivery":"queued"}}}"#
    );

    let waiting = entry(true);
    waiting.set_stage(Stage::Preparing);
    waiting.set_authorizing(Some("linear"));
    queue_two(&waiting);
    assert_eq!(
        waiting.state_json(),
        r#"{"last_seq":null,"queued":[{"submission_id":"sub-1","text":"first"},{"attachments":[{"kind":{"Binary":{"bytes":3}},"name":"notes.txt","path":"/work/.efference/a/attachments/notes.txt"}],"submission_id":"sub-2","text":"second"}],"starting":{"authorizing":"linear","cwd":"/work","failure":null,"id":"amber-heron","resume":true,"since":1754300000000,"stage":"preparing"},"status":{"condition":{"authorizing":"linear","kind":"starting","since":1754300000000,"stage":"preparing"},"footer":{"queued":[null,"session[calm-finch]"]},"id":"amber-heron"},"verbs":{"compact":{"accepted":false,"message":"session \"amber-heron\" is starting: its environment and MCP servers are being prepared, and only a turn is accepted until it has started"},"continue":{"accepted":false,"message":"session \"amber-heron\" is starting: its environment and MCP servers are being prepared, and only a turn is accepted until it has started"},"turn":{"accepted":true,"delivery":"queued"}}}"#
    );

    let failed = entry(false);
    queue_two(&failed);
    failed.failed("the env hook exited 1".into());
    if let Some(Outcome::Failed(failure)) = &mut failed.inner.lock().unwrap().outcome {
        failure.at = AT + 1;
    }
    assert_eq!(
        failed.state_json(),
        r#"{"last_seq":null,"queued":[],"starting":{"authorizing":null,"cwd":"/work","failure":{"at":1754300000001,"reason":"the env hook exited 1","withdrawn":[{"submission_id":"sub-1","text":"first"},{"attachments":[{"kind":{"Binary":{"bytes":3}},"name":"notes.txt","path":"/work/.efference/a/attachments/notes.txt"}],"submission_id":"sub-2","text":"second"}]},"id":"amber-heron","resume":false,"since":1754300000000,"stage":"environment"},"status":{"condition":{"kind":"failed_to_start","reason":"the env hook exited 1"},"footer":{},"id":"amber-heron"},"verbs":{"compact":{"accepted":false,"message":"session \"amber-heron\" did not start: the env hook exited 1"},"continue":{"accepted":false,"message":"session \"amber-heron\" did not start: the env hook exited 1"},"turn":{"accepted":false,"message":"session \"amber-heron\" did not start: the env hook exited 1"}}}"#
    );

    let stopped = entry(false);
    stopped.cancelled_by("archived");
    assert_eq!(
        stopped.state_json(),
        r#"{"last_seq":null,"queued":[],"starting":{"authorizing":null,"cwd":"/work","failure":null,"id":"amber-heron","resume":false,"since":1754300000000,"stage":"environment"},"status":{"condition":{"kind":"ended"},"footer":{},"id":"amber-heron"},"verbs":{"compact":{"accepted":false,"message":"session \"amber-heron\" was stopped while starting: archived"},"continue":{"accepted":false,"message":"session \"amber-heron\" was stopped while starting: archived"},"turn":{"accepted":false,"message":"session \"amber-heron\" was stopped while starting: archived"}}}"#
    );
}
