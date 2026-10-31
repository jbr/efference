//! The bytes each state topic serializes to (docs/control-api.md), pinned for the states
//! that exercise its shape: an optional field absent and present, a list empty and full,
//! each variant. The clients parse these bytes, so a change to them is a change to the
//! control channel, and it fails here first, as a change to the prompt's bytes fails the
//! wire pins (`projection_matches_expected_wire_bytes`).
//!
//! The comparison is of whole strings, key order included. Every topic reaches the channel
//! through `serde_json::Value`, which this crate builds without `preserve_order`, so an
//! object's keys come out sorted at every depth whatever order a struct declares its fields
//! in. A topic serialized any other way would reorder its keys and fail here.

use super::*;
use crate::api::{CacheMiss, CacheMissReason, PlanLimit, PlanSnapshot, PlanWindow};
use crate::config::{CommandStance, ConfigSource, testing::ConfigFixture};
use crate::log::attachment::{Arrival, Kind, Shape};
use crate::log::{AssistantOutput, ConfigChange, Spot, Stance, Telemetry, Timestamp};
use crate::policy::ReviewLevel;
use crate::protocol::agents::BoundAction;

/// An instant every pinned timestamp is built from: absolute Unix millis, as the channel
/// carries them.
const AT: u64 = 1_754_300_000_000;

/// The bytes a `state` frame carries for `topic`, the same as its key of `GET /state`.
fn bytes(shared: &Session, topic: Topic) -> String {
    shared.topic_json(topic).to_string()
}

/// A session whose log holds `payloads`, each entry stamped at `AT` plus its seq.
fn logged(payloads: Vec<Payload>) -> Arc<Session> {
    let entries = payloads
        .into_iter()
        .enumerate()
        .map(|(seq, payload)| {
            LogEntry::new(seq as u64, payload)
                .with_turn(1)
                .with_at(Timestamp(AT + seq as u64))
        })
        .collect();
    Session::from_log(Log::in_memory_with(entries)).0
}

fn arrival() -> Arrival {
    Arrival {
        name: "notes.txt".into(),
        path: "/work/.efference/a/attachments/notes.txt".into(),
        kind: Kind::Text {
            bytes: 12,
            lines: 1,
            shape: Shape::Prose,
            first_line: "hello there".into(),
        },
    }
}

/// Two queued turns: the operator's text, and an agent's turn carrying an attachment.
pub(super) fn queue_two(shared: &Session) {
    shared.submit_turn(SubmittedTurn::new("first", "sub-1"));
    shared.submit_turn(
        SubmittedTurn::new("second", "sub-2")
            .sourced(Some("session[calm-finch]".into()))
            .with_attachments(vec![arrival()]),
    );
}

fn usage(input: u64, output: u64, written: u64, read: u64) -> RequestUsage {
    RequestUsage::new(Usage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: written,
        cache_read_input_tokens: read,
    })
    .with_model("claude-haiku-4-5")
}

fn plan(observed_at: u64) -> PlanSnapshot {
    PlanSnapshot {
        plan_type: Some("plus".into()),
        active: Some("codex".into()),
        limits: BTreeMap::from([(
            "codex".to_string(),
            PlanLimit {
                name: None,
                primary: Some(PlanWindow {
                    used_percent: 12.5,
                    window_minutes: Some(300),
                    resets_at: Some(AT + 60_000),
                }),
                secondary: None,
            },
        )]),
        observed_at,
    }
}

#[test]
fn a_fresh_sessions_state_keeps_its_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(
        shared.state_json(),
        r#"{"ask":null,"cold":null,"compaction_failure":null,"config_repair":null,"current_operations":null,"gate":null,"last_seq":null,"loop_phase":{"phase":"idle"},"newer_version":null,"notifications":[],"pace":{"held":false,"speed_ms":0},"processes":[],"proposals":[],"queued":[],"resolved_review_level":"spotter_human","review_level":"spotter_human","role":{"commands":{},"paths":{},"scope":null,"source":null,"tools":{},"watch_for":[]},"session_identity":null,"stall":null,"starting":null,"status":{"condition":{"kind":"idle"},"footer":{},"id":""},"supervisor":null,"triage_upstream_failure":null,"upstream_failure":null,"upstream_retry":null,"usage":{"cold_streak":0,"context":null,"cost":null,"cumulative":{"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"input_tokens":0,"output_tokens":0},"keepalive_skipped":null,"last":null,"last_cache_miss":null,"last_request_at":null,"plan":null,"retention":{"kept_lines":0,"shown_lines":0,"truncated_lines":0,"withheld":0},"triage_cold_streak":null,"triage_keepalive_skipped":null,"triage_plan":null},"verbs":{"compact":{"accepted":false,"message":"nothing to compact: the conversation is already shorter than what a compaction keeps verbatim"},"continue":{"accepted":false,"message":"nothing to continue: the session has no turns yet"},"turn":{"accepted":true,"delivery":"now"}}}"#
    );
    assert_eq!(
        shared.loop_json(),
        r#"{"loop_phase":{"phase":"idle"},"pace":{"held":false,"speed_ms":0},"review_level":"spotter_human"}"#
    );
}

#[test]
fn the_ask_topic_keeps_its_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(bytes(&shared, Topic::Ask), "null");
    let _ask = shared.begin_ask("toolu_ask", "Which branch?".into(), 1, 1);
    assert_eq!(
        bytes(&shared, Topic::Ask),
        r#"{"call_id":"toolu_ask","n":1,"of":1,"question":"Which branch?","recalled":[]}"#
    );

    let (shared, _turns) = Session::in_memory();
    queue_two(&shared);
    let _ask = shared.begin_ask("toolu_ask", "Which branch?".into(), 2, 3);
    assert_eq!(
        bytes(&shared, Topic::Ask),
        r#"{"call_id":"toolu_ask","n":2,"of":3,"question":"Which branch?","recalled":[{"submission_id":"sub-1","text":"first"},{"attachments":[{"kind":{"Text":{"bytes":12,"first_line":"hello there","lines":1,"shape":"Prose"}},"name":"notes.txt","path":"/work/.efference/a/attachments/notes.txt"}],"submission_id":"sub-2","text":"second"}]}"#
    );
    assert_eq!(bytes(&shared, Topic::Queued), "[]");
    assert_eq!(
        bytes(&shared, Topic::Status),
        r#"{"condition":{"kind":"asking","n":2,"of":3,"question":"Which branch?","returned":[{"text":"first"},{"source":"session[calm-finch]","text":"second"}]},"footer":{},"id":""}"#
    );
}

#[test]
fn the_queued_topic_keeps_its_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(bytes(&shared, Topic::Queued), "[]");
    queue_two(&shared);
    assert_eq!(
        bytes(&shared, Topic::Queued),
        r#"[{"submission_id":"sub-1","text":"first"},{"attachments":[{"kind":{"Text":{"bytes":12,"first_line":"hello there","lines":1,"shape":"Prose"}},"name":"notes.txt","path":"/work/.efference/a/attachments/notes.txt"}],"submission_id":"sub-2","text":"second"}]"#
    );
}

#[test]
fn the_usage_topic_keeps_its_bytes() {
    // Every field set: a context window, a cold streak, the triage client's cells, a
    // priced session, a plan, an attributed miss, and both keepalive stances.
    let (shared, _turns) = Session::in_memory();
    shared.set_capabilities(crate::api::ModelCapabilities {
        context_length: Some(200_000),
        ..crate::api::ModelCapabilities::unknown()
    });
    let triage_plan = crate::api::plan_state();
    *triage_plan.lock().unwrap() = Some(plan(AT + 5));
    shared.set_triage_client((upstream_failure_state(), triage_plan));
    shared.record_request(RequestOrigin::Turn, usage(10, 20, 30, 0));
    shared.record_request(RequestOrigin::Turn, usage(10, 20, 30, 0));
    shared.record_request(
        RequestOrigin::Turn,
        usage(11, 21, 31, 0).with_cache_miss(CacheMiss {
            reason: CacheMissReason::ToolsChanged,
            cache_missed_input_tokens: Some(12_280),
        }),
    );
    shared.record_request(RequestOrigin::Spot, usage(1, 2, 0, 3));
    shared.enable_cost_tracking(crate::api::Endpoint::at(
        crate::log::Provider::OpenRouter,
        "http://127.0.0.1:9/api/v1/messages",
    ));
    {
        let mut cost = shared.cost.lock().unwrap();
        cost.total_usd = 0.0421;
        cost.unpriced = 1;
    }
    *shared.plan.lock().unwrap() = Some(plan(AT));
    {
        let mut metrics = shared.metrics.lock().unwrap();
        metrics.last_request_wall = Some(Timestamp(AT));
        metrics.last_cache_miss.as_mut().unwrap().at = Timestamp(AT + 1);
        metrics.keepalive_skipped = Some(crate::protocol::state::KeepaliveSkip {
            at: Timestamp(AT + 2),
            why: crate::protocol::state::KeepaliveSkipReason::Discard {
                output_tokens: 10_000,
                completion_usd: Some(0.005),
                prefix_tokens: 5_000,
                premium_usd: None,
            },
        });
        metrics.triage_keepalive_skipped = Some(crate::protocol::state::KeepaliveSkip {
            at: Timestamp(AT + 3),
            why: crate::protocol::state::KeepaliveSkipReason::Exhausted {
                detail: "usage limit reached".into(),
                resets_at: Some(AT + 3_600_000),
            },
        });
    }
    assert_eq!(
        bytes(&shared, Topic::Usage),
        r#"{"cold_streak":2,"context":{"limit":200000,"used":42},"cost":{"unpriced":1,"usd":0.0421},"cumulative":{"cache_creation_input_tokens":91,"cache_read_input_tokens":3,"input_tokens":32,"output_tokens":63},"keepalive_skipped":{"at":1754300000002,"completion_usd":0.005,"output_tokens":10000,"prefix_tokens":5000,"premium_usd":null,"reason":"discard"},"last":{"cache_creation_input_tokens":0,"cache_read_input_tokens":3,"input_tokens":1,"output_tokens":2},"last_cache_miss":{"at":1754300000001,"cache_missed_input_tokens":12280,"reason":"tools_changed"},"last_request_at":1754300000000,"plan":{"active":"codex","limits":{"codex":{"name":null,"primary":{"resets_at":1754300060000,"used_percent":12.5,"window_minutes":300},"secondary":null}},"observed_at":1754300000000,"type":"plus"},"retention":{"kept_lines":0,"shown_lines":0,"truncated_lines":0,"withheld":0},"triage_cold_streak":0,"triage_keepalive_skipped":{"at":1754300000003,"detail":"usage limit reached","reason":"exhausted","resets_at":1754303600000},"triage_plan":{"active":"codex","limits":{"codex":{"name":null,"primary":{"resets_at":1754300060000,"used_percent":12.5,"window_minutes":300},"secondary":null}},"observed_at":1754300000005,"type":"plus"}}"#
    );

    // A window the provider did not report, an unpriced-free cost, a triage client with
    // no plan, a miss with no estimate, and the stances' other reasons.
    let (shared, _turns) = Session::in_memory();
    shared.set_capabilities(crate::api::ModelCapabilities::unknown());
    shared.set_triage_client((upstream_failure_state(), crate::api::plan_state()));
    shared.record_request(
        RequestOrigin::Turn,
        usage(5, 6, 0, 7).with_cache_miss(CacheMiss {
            reason: CacheMissReason::PreviousMessageNotFound,
            cache_missed_input_tokens: None,
        }),
    );
    shared.enable_cost_tracking(crate::api::Endpoint::at(
        crate::log::Provider::OpenRouter,
        "http://127.0.0.1:9/api/v1/messages",
    ));
    {
        let mut metrics = shared.metrics.lock().unwrap();
        metrics.last_request_wall = Some(Timestamp(AT));
        metrics.last_cache_miss.as_mut().unwrap().at = Timestamp(AT + 1);
        metrics.keepalive_skipped = Some(crate::protocol::state::KeepaliveSkip {
            at: Timestamp(AT + 2),
            why: crate::protocol::state::KeepaliveSkipReason::Rejected {
                detail: "400 bad request".into(),
            },
        });
        metrics.triage_keepalive_skipped = Some(crate::protocol::state::KeepaliveSkip {
            at: Timestamp(AT + 3),
            why: crate::protocol::state::KeepaliveSkipReason::Exhausted {
                detail: "usage limit reached".into(),
                resets_at: None,
            },
        });
    }
    assert_eq!(
        bytes(&shared, Topic::Usage),
        r#"{"cold_streak":0,"context":{"limit":null,"used":12},"cost":{"unpriced":0,"usd":0.0},"cumulative":{"cache_creation_input_tokens":0,"cache_read_input_tokens":7,"input_tokens":5,"output_tokens":6},"keepalive_skipped":{"at":1754300000002,"detail":"400 bad request","reason":"rejected"},"last":{"cache_creation_input_tokens":0,"cache_read_input_tokens":7,"input_tokens":5,"output_tokens":6},"last_cache_miss":{"at":1754300000001,"reason":"previous_message_not_found"},"last_request_at":1754300000000,"plan":null,"retention":{"kept_lines":0,"shown_lines":0,"truncated_lines":0,"withheld":0},"triage_cold_streak":0,"triage_keepalive_skipped":{"at":1754300000003,"detail":"usage limit reached","reason":"exhausted","resets_at":null},"triage_plan":null}"#
    );
}

#[test]
fn the_role_topic_keeps_its_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(
        bytes(&shared, Topic::Role),
        r#"{"commands":{},"paths":{},"scope":null,"source":null,"tools":{},"watch_for":[]}"#
    );

    let mut config = ConfigFixture::new()
        .with_scope("Fix the parser.")
        .with_watch_for("deleting tests")
        .with_watch_for("force pushes")
        .with_command("git", CommandStance::Clear)
        .with_command("git push", CommandStance::Deny)
        .with_command("cargo", CommandStance::Watch)
        .with_command("make", CommandStance::Gate)
        .with_path("src", true, false)
        .with_path("/tmp/scratch", true, true)
        .with_tool("read")
        .with_namespace("lsp")
        .build();
    // An entry that sets neither field reads as an empty object.
    config
        .tools
        .namespaces
        .insert("ferritin".into(), Default::default());
    let (shared, _turns) = Session::in_memory_with_role(LoadedConfig {
        role: config,
        source: ConfigSource::Path("/work/efference.toml".into()),
    });
    assert_eq!(
        bytes(&shared, Topic::Role),
        r#"{"commands":{"cargo":"watch","git":"clear","git push":"deny","make":"gate"},"paths":{"/tmp/scratch":{"read":true,"write":true},"src":{"read":true,"write":false}},"scope":"Fix the parser.","source":"/work/efference.toml","tools":{"efference":{"enabled":true},"efference.read":{"granted":true},"ferritin":{},"lsp":{"enabled":true}},"watch_for":["deleting tests","force pushes"]}"#
    );
}

#[test]
fn the_pace_topic_and_the_loop_resource_keep_their_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(
        bytes(&shared, Topic::Pace),
        r#"{"held":false,"speed_ms":0}"#
    );
    shared.set_pace(Some(Duration::from_millis(2500)), Some(true));
    shared.set_review_level(ReviewLevel::HumanOnly);
    shared.set_phase(LoopPhase::Delayed { until: AT });
    assert_eq!(
        bytes(&shared, Topic::Pace),
        r#"{"held":true,"speed_ms":2500}"#
    );
    assert_eq!(
        shared.loop_json(),
        r#"{"loop_phase":{"phase":"delayed","until":1754300000000},"pace":{"held":true,"speed_ms":2500},"review_level":"human_only"}"#
    );
}

#[test]
fn the_verbs_topic_keeps_its_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(
        bytes(&shared, Topic::Verbs),
        r#"{"compact":{"accepted":false,"message":"nothing to compact: the conversation is already shorter than what a compaction keeps verbatim"},"continue":{"accepted":false,"message":"nothing to continue: the session has no turns yet"},"turn":{"accepted":true,"delivery":"now"}}"#
    );

    shared.append(1, user("go")).unwrap();
    assert_eq!(
        bytes(&shared, Topic::Verbs),
        r#"{"compact":{"accepted":false,"message":"nothing to compact: the conversation is already shorter than what a compaction keeps verbatim"},"continue":{"accepted":true},"turn":{"accepted":true,"delivery":"now"}}"#
    );
    shared.set_phase(LoopPhase::Requesting);
    assert_eq!(
        bytes(&shared, Topic::Verbs),
        r#"{"compact":{"accepted":false,"message":"the loop is running — compact when it stops"},"continue":{"accepted":false,"message":"the loop is already running — there is nothing to hand back"},"turn":{"accepted":true,"delivery":"queued"}}"#
    );

    let (shared, _turns) = Session::in_memory();
    shared.set_cache_ttl(Duration::from_secs(3_600));
    append_compactable_history(&shared);
    shared.record_request(RequestOrigin::Turn, usage(1, 1, 0, 0));
    assert_eq!(
        bytes(&shared, Topic::Verbs),
        r#"{"compact":{"accepted":true,"warm":true},"continue":{"accepted":true},"turn":{"accepted":true,"delivery":"now"}}"#
    );

    let (shared, _turns) = Session::in_memory();
    shared.set_cache_ttl(Duration::from_secs(300));
    append_compactable_history(&shared);
    shared.metrics.lock().unwrap().last_request_wall = Some(Timestamp(AT));
    assert_eq!(
        bytes(&shared, Topic::Verbs),
        r#"{"compact":{"accepted":true,"warm":false},"continue":{"accepted":true},"turn":{"accepted":true,"delivery":"now"}}"#
    );
    let _ask = shared.begin_ask("toolu_ask", "Which?".into(), 1, 1);
    assert_eq!(
        bytes(&shared, Topic::Verbs),
        r#"{"compact":{"accepted":false,"message":"a question is pending — answer it first"},"continue":{"accepted":false,"message":"a question is pending — answer it with a turn to continue"},"turn":{"accepted":true,"delivery":"now"}}"#
    );
}

#[test]
fn the_cold_topic_keeps_its_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(bytes(&shared, Topic::Cold), "null");

    shared.set_cache_ttl(Duration::from_secs(300));
    append_compactable_history(&shared);
    shared.metrics.lock().unwrap().last_request_wall = Some(Timestamp(AT));
    assert_eq!(
        bytes(&shared, Topic::Cold),
        r#"{"context_tokens":null,"since":1754300300000}"#
    );

    shared.record_request(RequestOrigin::Turn, usage(100, 5, 200, 300));
    shared.metrics.lock().unwrap().last_request_wall = Some(Timestamp(AT));
    assert_eq!(
        bytes(&shared, Topic::Cold),
        r#"{"context_tokens":600,"since":1754300300000}"#
    );
}

/// The open operations and the compaction's failure, from logs whose entries carry fixed
/// instants.
#[test]
fn the_operations_topics_keep_their_bytes() {
    let fresh = logged(vec![]);
    assert_eq!(bytes(&fresh, Topic::CurrentOperations), "null");
    assert_eq!(bytes(&fresh, Topic::CompactionFailure), "null");

    let stage = |request, origin, stage| {
        Payload::Telemetry(Telemetry::RequestStage(crate::log::RequestStage::new(
            request, origin, stage,
        )))
    };
    let calls = Payload::assistant(AssistantOutput::from_legacy_messages(
        vec![],
        vec![],
        vec![
            ToolCall::new(
                crate::bash::NAME,
                "toolu_1",
                serde_json::json!({"command": "cargo test", "intent": "run the suite"}),
            ),
            ToolCall::new("read", "toolu_2", serde_json::json!({"path": "src/lib.rs"})),
        ],
    ));
    let busy = logged(vec![
        stage(0, RequestOrigin::Turn, crate::log::RequestStageKind::Sent),
        stage(1, RequestOrigin::Triage, crate::log::RequestStageKind::Sent),
        stage(
            1,
            RequestOrigin::Triage,
            crate::log::RequestStageKind::FirstByte,
        ),
        stage(2, RequestOrigin::Spot, crate::log::RequestStageKind::Sent),
        stage(
            2,
            RequestOrigin::Spot,
            crate::log::RequestStageKind::AttemptFailed {
                attempt: 2,
                status: 429,
            },
        ),
        calls,
        Payload::Telemetry(Telemetry::call_started("toolu_1")),
        Payload::Telemetry(Telemetry::call_started("toolu_2")),
        Payload::Telemetry(Telemetry::CompactionRequested { warm: true, cut: 3 }),
    ]);
    assert_eq!(
        bytes(&busy, Topic::CurrentOperations),
        r#"{"compaction":{"cut":3,"since":1754300000008,"state":"waiting","warm":true},"requests":[{"origin":"turn","request":0,"since":1754300000000,"stage":"sent"},{"origin":"triage","request":1,"since":1754300000002,"stage":"streaming"},{"attempt":2,"origin":"spot","request":2,"since":1754300000004,"stage":"retrying","status":429}],"tools":[{"call_id":"toolu_1","command":"cargo test","intent":"run the suite","since":1754300000006,"tool":"bash"},{"call_id":"toolu_2","intent":null,"since":1754300000007,"tool":"read"}]}"#
    );

    let writing = logged(vec![
        Payload::Telemetry(Telemetry::CompactionRequested {
            warm: false,
            cut: 7,
        }),
        stage(
            4,
            RequestOrigin::Compaction,
            crate::log::RequestStageKind::Sent,
        ),
    ]);
    assert_eq!(
        bytes(&writing, Topic::CurrentOperations),
        r#"{"compaction":{"cut":7,"since":1754300000001,"state":"writing","warm":false},"requests":[{"origin":"compaction","request":4,"since":1754300000001,"stage":"sent"}],"tools":[]}"#,
    );

    let failed = logged(vec![
        Payload::Telemetry(Telemetry::CompactionRequested {
            warm: false,
            cut: 7,
        }),
        Payload::Telemetry(Telemetry::CompactionFailed {
            error: "the summary request failed: 529 overloaded".into(),
        }),
    ]);
    assert_eq!(
        bytes(&failed, Topic::CompactionFailure),
        r#"{"at":1754300000001,"error":"the summary request failed: 529 overloaded"}"#,
    );
    assert_eq!(bytes(&failed, Topic::CurrentOperations), "null",);
}

#[test]
fn the_newer_version_topic_keeps_its_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(bytes(&shared, Topic::NewerVersion), "null");
    let cell = Arc::default();
    shared.set_newer_version(Arc::clone(&cell));
    cell.set(Some(crate::update_check::Release {
        version: "0.2.0".into(),
        tag: "v0.2.0".into(),
        url: "https://efference.net/releases/v0.2.0".into(),
    }));
    assert_eq!(
        bytes(&shared, Topic::NewerVersion),
        r#"{"tag":"v0.2.0","update_command":"curl -fsSL https://efference.net/install.sh | sh","url":"https://efference.net/releases/v0.2.0","version":"0.2.0"}"#
    );
}

#[test]
fn the_gate_topic_keeps_its_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(bytes(&shared, Topic::Gate), "null");
    let call = bash_call("ls");
    let _held = shared.register_pending(&call, Default::default());
    assert_eq!(
        bytes(&shared, Topic::Gate),
        r#"{"call_id":"toolu_x","input":{"command":"ls"},"name":"bash"}"#
    );
    assert_eq!(
        bytes(&shared, Topic::Status),
        r#"{"condition":{"call_id":"toolu_x","kind":"gated","reviewer":{"kind":"operator"},"summary":"ls","tool":"bash"},"footer":{},"id":""}"#
    );

    // A spotter's reason, a bound decision, a grant, and two escalations.
    let (shared, _turns) = Session::in_memory();
    shared
        .append(
            1,
            Payload::Spot(Spot::new("toolu_x", Stance::Gate, "it reaches the network")),
        )
        .unwrap();
    let bound = BoundAction {
        name: crate::bash::NAME.into(),
        input: serde_json::json!({"command": "curl -sI https://example.com", "intent": "fetch headers"}),
        intent: Some("fetch headers".into()),
        cwd: "/work/child".into(),
        project: "/work".into(),
        session: "child".into(),
        chain: vec!["child".into(), "grand".into()],
    };
    let grant = crate::policy::GateGrant {
        change: ConfigChange::Command {
            command: "git push".into(),
        },
        label: "always allow git push".into(),
    };
    let _held = shared.register_pending(
        &call,
        GateOptions::default()
            .with_bound(Some(&bound))
            .with_grant(Some(grant)),
    );
    shared.escalate(
        &call.id,
        Some("session[calm-finch]".into()),
        Some("parent".into()),
        Some("over to you".into()),
    );
    shared.escalate(&call.id, Some("session[parent]".into()), None, None);
    assert_eq!(
        bytes(&shared, Topic::Gate),
        r#"{"acting_on":{"chain":["child","grand"],"cwd":"/work/child","input":{"command":"curl -sI https://example.com","intent":"fetch headers"},"intent":"fetch headers","name":"bash","project":"/work","session":"child"},"call_id":"toolu_x","escalations":[{"note":"over to you","source":"session[calm-finch]","to":"parent"},{"source":"session[parent]","to":null}],"grant":{"change":{"command":{"command":"git push"}},"label":"always allow git push"},"input":{"command":"ls"},"name":"bash","reason":"it reaches the network"}"#
    );
    assert_eq!(
        bytes(&shared, Topic::Status),
        r#"{"condition":{"call_id":"toolu_x","escalations":[{"note":"over to you","source":"session[calm-finch]","to":"parent"},{"source":"session[parent]","to":null}],"held_because":"it reaches the network","kind":"gated","reviewer":{"kind":"operator"},"summary":"child's bash `curl -sI https://example.com`","tool":"bash"},"footer":{},"id":""}"#
    );
}

#[test]
fn the_inbox_topics_keep_their_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(bytes(&shared, Topic::Notifications), "[]");
    assert_eq!(bytes(&shared, Topic::Proposals), "[]");
    shared.notify(Notification::new("ci", "build failed", None));
    shared.notify(Notification::new(
        "ci",
        "flaky test",
        Some("ci:main".to_string()),
    ));
    shared.notify(Notification::new("session[child]", "child is idle", None).waking());
    shared
        .append(
            1,
            proposed_edit(ConfigChange::Path {
                path: "/tmp/scratch".into(),
                read: true,
                write: false,
            }),
        )
        .unwrap();
    assert_eq!(
        bytes(&shared, Topic::Notifications),
        r#"[{"body":"build failed","seq":0,"source":"ci"},{"body":"child is idle","seq":1,"source":"session[child]","wake":true},{"body":"flaky test","slot":"ci:main","source":"ci"}]"#
    );
    assert_eq!(
        bytes(&shared, Topic::Proposals),
        r#"[{"change":{"path":{"path":"/tmp/scratch","read":true,"write":false}},"seq":2}]"#
    );
}

#[test]
fn the_phase_and_review_topics_keep_their_bytes() {
    let (shared, _turns) = Session::in_memory();
    for (phase, expected) in [
        (LoopPhase::Idle, r#"{"phase":"idle"}"#),
        (LoopPhase::Requesting, r#"{"phase":"requesting"}"#),
        (LoopPhase::Executing, r#"{"phase":"executing"}"#),
        (
            LoopPhase::Delayed { until: AT },
            r#"{"phase":"delayed","until":1754300000000}"#,
        ),
        (LoopPhase::Gated, r#"{"phase":"gated"}"#),
        (LoopPhase::Asking, r#"{"phase":"asking"}"#),
        (LoopPhase::ColdParked, r#"{"phase":"cold_parked"}"#),
        (
            LoopPhase::Blocked {
                reason: BlockedReason::TriageUnavailable,
            },
            r#"{"phase":"blocked","reason":"triage_unavailable"}"#,
        ),
        (
            LoopPhase::Blocked {
                reason: BlockedReason::TriageDefect,
            },
            r#"{"phase":"blocked","reason":"triage_defect"}"#,
        ),
        (
            LoopPhase::Blocked {
                reason: BlockedReason::ConfigRepair,
            },
            r#"{"phase":"blocked","reason":"config_repair"}"#,
        ),
        (
            LoopPhase::Blocked {
                reason: BlockedReason::PaceHeld,
            },
            r#"{"phase":"blocked","reason":"pace_held"}"#,
        ),
    ] {
        shared.set_phase(phase);
        assert_eq!(bytes(&shared, Topic::LoopPhase), expected);
    }
    // The dial, and the position in force: `supervisor` with no live supervisor reviews
    // as `spotter_human`.
    for (level, dial, resolved) in [
        (
            ReviewLevel::Autonomous,
            r#""autonomous""#,
            r#""autonomous""#,
        ),
        (
            ReviewLevel::Supervisor,
            r#""supervisor""#,
            r#""spotter_human""#,
        ),
        (
            ReviewLevel::SpotterHuman,
            r#""spotter_human""#,
            r#""spotter_human""#,
        ),
        (ReviewLevel::HumanOnly, r#""human_only""#, r#""human_only""#),
    ] {
        shared.set_review_level(level);
        assert_eq!(bytes(&shared, Topic::ReviewLevel), dial);
        assert_eq!(bytes(&shared, Topic::ResolvedReviewLevel), resolved);
    }
    assert_eq!(bytes(&shared, Topic::Supervisor), "null");
}

#[test]
fn the_stall_and_repair_topics_keep_their_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(bytes(&shared, Topic::Stall), "null");
    assert_eq!(bytes(&shared, Topic::ConfigRepair), "null");
    shared.set_stall(Stall {
        reason: StallReason::RetriesExhausted,
        attempts: Some(12),
        at: AT,
        message: "the trajectory stopped: 12 attempts in a row failed upstream".into(),
        detail: "upstream SSE stream failed mid-stream: connection reset".into(),
    });
    assert_eq!(
        bytes(&shared, Topic::Stall),
        r#"{"at":1754300000000,"attempts":12,"detail":"upstream SSE stream failed mid-stream: connection reset","message":"the trajectory stopped: 12 attempts in a row failed upstream","reason":"retries_exhausted"}"#
    );
    assert_eq!(
        bytes(&shared, Topic::Status),
        r#"{"condition":{"held_triage":false,"kind":"stalled","message":"the trajectory stopped: 12 attempts in a row failed upstream"},"footer":{},"id":""}"#
    );
    shared.set_stall(Stall {
        reason: StallReason::TriageDefect,
        attempts: None,
        at: AT,
        message: "the triage never produced a usable verdict".into(),
        detail: "no verdict".into(),
    });
    assert_eq!(
        bytes(&shared, Topic::Stall),
        r#"{"at":1754300000000,"attempts":null,"detail":"no verdict","message":"the triage never produced a usable verdict","reason":"triage_defect"}"#
    );
    shared.require_config_repair(ConfigRepair {
        path: "/work/.efference/a/config.toml".into(),
        at: AT,
        message: "config unparseable — the loop is parked until a human repairs the file".into(),
        parse_error: "expected `=`".into(),
        restore_error: "permission denied".into(),
    });
    assert_eq!(
        bytes(&shared, Topic::ConfigRepair),
        r#"{"at":1754300000000,"message":"config unparseable — the loop is parked until a human repairs the file","parse_error":"expected `=`","path":"/work/.efference/a/config.toml","restore_error":"permission denied"}"#
    );
}

#[test]
fn the_identity_topic_keeps_its_bytes() {
    let (shared, _turns) = Session::in_memory();
    assert_eq!(bytes(&shared, Topic::SessionIdentity), "null");
    shared.set_identity(SessionIdentity {
        id: "amber-heron".into(),
        title: None,
        model: "anthropic/claude-opus-4".into(),
        cwd: "/work".into(),
        inference: None,
        successor: None,
    });
    assert_eq!(
        bytes(&shared, Topic::SessionIdentity),
        r#"{"cwd":"/work","id":"amber-heron","model":"anthropic/claude-opus-4"}"#
    );
    let selection = |model: &str| crate::user_config::ModelSelection {
        connection: "work".into(),
        provider: crate::log::Provider::OpenRouter,
        model: model.into(),
    };
    shared.set_identity(SessionIdentity {
        id: "amber-heron".into(),
        title: Some("Session names: slugs and titles".into()),
        model: "anthropic/claude-opus-4".into(),
        cwd: "/work".into(),
        inference: Some(crate::inference::InferenceIdentity {
            main: selection("anthropic/claude-opus-4"),
            spotter: selection("anthropic/claude-haiku-4-5"),
            triage: crate::inference::TriageIdentity {
                shared: true,
                selection: selection("anthropic/claude-opus-4"),
            },
        }),
        successor: Some("calm-finch".into()),
    });
    assert_eq!(
        bytes(&shared, Topic::SessionIdentity),
        r#"{"cwd":"/work","id":"amber-heron","inference":{"main":{"connection":"work","model":"anthropic/claude-opus-4","provider":"openrouter"},"spotter":{"connection":"work","model":"anthropic/claude-haiku-4-5","provider":"openrouter"},"triage":{"connection":"work","model":"anthropic/claude-opus-4","provider":"openrouter","shared":true}},"model":"anthropic/claude-opus-4","successor":"calm-finch","title":"Session names: slugs and titles"}"#
    );
    assert_eq!(
        bytes(&shared, Topic::Status),
        r#"{"condition":{"kind":"idle"},"footer":{"model":"anthropic/claude-opus-4"},"id":"amber-heron"}"#
    );
}
