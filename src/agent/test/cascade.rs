//! Tests for the approval cascade, with the test doubles it needs.
use super::*;

/// A frontend that must not be reached. A call naming no tool cannot run, so nothing
/// on the permission path — human gate included — should be asked about it.
struct NeverGates;

impl Frontend for NeverGates {
    async fn read_user_turn(&mut self) -> Option<SubmittedTurn> {
        unreachable!("the loop is not running")
    }
    async fn approve(&mut self, call: &ToolCall, _gate: GateOptions<'_>) -> Approval {
        panic!(
            "a call naming no tool reached the human gate: {}",
            call.name
        )
    }
    async fn interrupted(&mut self) -> crate::tools::Interrupt {
        unreachable!("no tool runs")
    }
}

/// The spotter rung, which must not be consulted either: asked about a nonexistent tool it
/// returns `clear`, with a rationale invented for the call it imagined
/// (journal/2026-08-07-session-key-review.md).
struct NeverAsked;

impl Spotter for NeverAsked {
    async fn assess(
        &self,
        _role: &crate::config::Config,
        call: &ToolCall,
        _brief: Option<&crate::tools::ToolBrief<'_>>,
        _chain: &[String],
        _probe: Option<&str>,
        _precedents: &[ToolCall],
    ) -> crate::curation::spotter::Spotting {
        panic!("a call naming no tool reached the spotter: {}", call.name)
    }
}

/// A frontend that records what the gate was asked to decide, for the bound
/// `agents__decide` ladder tests and the gate-grant tests. It answers with a fixed
/// decision.
pub(super) struct RecordingFrontend {
    pub(super) decisions: Vec<(String, Option<crate::protocol::agents::BoundAction>)>,
    /// The grant each gate offered, in the order the gates were reached.
    pub(super) grants: Vec<Option<policy::GateGrant>>,
    decision: Approval,
}

impl RecordingFrontend {
    /// A frontend that answers every gate with `decision`.
    pub(super) fn answering(decision: Approval) -> Self {
        Self {
            decisions: Vec::new(),
            grants: Vec::new(),
            decision,
        }
    }
}

impl Frontend for RecordingFrontend {
    async fn read_user_turn(&mut self) -> Option<SubmittedTurn> {
        unreachable!("the loop is not running")
    }
    async fn approve(&mut self, call: &ToolCall, gate: GateOptions<'_>) -> Approval {
        self.decisions
            .push((call.name.clone(), gate.bound.cloned()));
        self.grants.push(gate.grant);
        self.decision.clone()
    }
    async fn interrupted(&mut self) -> crate::tools::Interrupt {
        unreachable!("no tool runs")
    }
}

/// A spotter that records the move it was asked about, so a test can pin that the
/// parent's rung judged the child's leaf rather than the `agents__decide` call.
struct RecordingSpotter {
    seen: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    stance: Stance,
    reason: String,
}

impl Spotter for RecordingSpotter {
    async fn assess(
        &self,
        _role: &crate::config::Config,
        call: &ToolCall,
        _brief: Option<&crate::tools::ToolBrief<'_>>,
        chain: &[String],
        _probe: Option<&str>,
        _precedents: &[ToolCall],
    ) -> crate::curation::spotter::Spotting {
        self.seen
            .lock()
            .unwrap()
            .push((call.name.clone(), chain.to_vec()));
        crate::curation::spotter::Spotting {
            read: crate::curation::spotter::Read {
                stance: self.stance,
                reason: self.reason.clone(),
            },
            origin: Origin::Model,
            axes: None,
            usage: None,
        }
    }
}

/// A session whose effective policy is `role` — the parent in a bound-approve test.
pub(super) fn parent_with(role: crate::config::Config) -> Arc<Session> {
    let (shared, _rx) = Session::in_memory_with_role(crate::config::LoadedConfig {
        role,
        ..crate::config::LoadedConfig::default()
    });
    shared
}

/// The `agents__decide` call the parent's model wrote, naming the child, the held
/// call's handle, and `decision`.
fn decide_call(decision: &str) -> ToolCall {
    ToolCall::new(
        crate::protocol::agents::DECIDE_WIRE_NAME,
        "toolu_parent",
        serde_json::json!({ "agent": "child", "seq": "4", "decision": decision }),
    )
}

/// The allowing `agents__decide` the bound-ladder tests judge.
pub(super) fn approve_call() -> ToolCall {
    decide_call("allow")
}

/// A leaf the child holds, bound to the parent's approval.
pub(super) fn bound_leaf(command: &str) -> crate::protocol::agents::BoundAction {
    crate::protocol::agents::BoundAction {
        name: BASH.into(),
        input: serde_json::json!({ "command": command, "intent": "whatever" }),
        intent: Some("whatever".into()),
        cwd: std::env::current_dir().unwrap(),
        project: std::env::current_dir().unwrap(),
        session: "child".into(),
        chain: vec!["child".into()],
    }
}

/// A granted `agents__decide` clears blind (docs/permissions.md): that grant is
/// the config's statement of trust in the coordinator, so the child's leaf is not
/// re-litigated and neither rung is consulted.
#[tokio::test]
async fn a_granted_decide_clears_blind() {
    let mut role = crate::config::Config::default();
    role.tools.grant(crate::protocol::agents::DECIDE_WIRE_NAME);
    let shared = parent_with(role);
    let mut frontend = RecordingFrontend::answering(Approval::Abort);
    let spotter = crate::curation::spotter::StaticSpotter {
        stance: Stance::Gate,
        reason: "would gate".into(),
    };
    let approval = judge(
        &mut frontend,
        &shared,
        &spotter,
        &Toolbox::bash_only(),
        1,
        &approve_call(),
        Some(&bound_leaf("rm -rf /")),
    )
    .await
    .unwrap();
    assert_eq!(approval, Approval::Approve);
    assert!(
        frontend.decisions.is_empty(),
        "a granted approve clears before the gate"
    );
}

/// An ungranted approve on a leaf the parent's fast path clears (a `[commands]` grant)
/// also clears (docs/permissions.md): the ladder runs on the leaf, under the parent's
/// config, with the leaf's cwd.
#[tokio::test]
async fn an_ungranted_approve_clears_on_the_leafs_fast_path() {
    let mut role = crate::config::Config::default();
    role.commands
        .insert("cargo".into(), crate::config::CommandStance::Clear);
    let shared = parent_with(role);
    let mut frontend = RecordingFrontend::answering(Approval::Abort);
    let spotter = crate::curation::spotter::StaticSpotter {
        stance: Stance::Gate,
        reason: "would gate".into(),
    };
    let approval = judge(
        &mut frontend,
        &shared,
        &spotter,
        &Toolbox::bash_only(),
        1,
        &approve_call(),
        Some(&bound_leaf("cargo test")),
    )
    .await
    .unwrap();
    assert_eq!(approval, Approval::Approve);
    assert!(
        frontend.decisions.is_empty(),
        "the leaf cleared on the fast path, so no gate was reached"
    );
    assert!(
        !shared
            .effective_config()
            .tools
            .granted(crate::protocol::agents::DECIDE_WIRE_NAME),
        "the approval tool itself was never granted"
    );
}

/// A leaf outside the parent's grants gates, and what reaches the gate is the leaf,
/// carried on the parent's own approval call (docs/permissions.md).
#[tokio::test]
async fn a_leaf_outside_the_grants_gates_with_the_bound_action() {
    let shared = parent_with(crate::config::Config::default());
    let mut frontend = RecordingFrontend::answering(Approval::Approve);
    let spotter = crate::curation::spotter::StaticSpotter {
        stance: Stance::Gate,
        reason: "outside the mission".into(),
    };
    let bound = bound_leaf("rm -rf ./build");
    let approval = judge(
        &mut frontend,
        &shared,
        &spotter,
        &Toolbox::bash_only(),
        1,
        &approve_call(),
        Some(&bound),
    )
    .await
    .unwrap();
    assert_eq!(approval, Approval::Approve);
    assert_eq!(frontend.decisions.len(), 1, "the parent's gate was reached");
    let (name, seen) = &frontend.decisions[0];
    assert_eq!(
        name,
        crate::protocol::agents::DECIDE_WIRE_NAME,
        "the gate registers the parent's call"
    );
    assert_eq!(
        seen.as_ref(),
        Some(&bound),
        "the gate carries the child's held leaf action"
    );
    // The audit entry correlates to the parent's held call, not the child's.
    assert!(
        shared.snapshot().iter().any(|e| matches!(
            &e.payload,
            Payload::Spot(s) if s.call_id.0 == "toolu_parent"
        )),
        "the spotter's verdict is logged against the approval the parent holds"
    );
}

/// The parent's spotter judges the child's leaf and its chain, never the
/// `agents__decide` call (docs/permissions.md).
#[tokio::test]
async fn the_spotter_judges_the_leaf_with_the_chain() {
    let shared = parent_with(crate::config::Config::default());
    let mut frontend = RecordingFrontend::answering(Approval::Approve);
    let spotter = RecordingSpotter {
        seen: std::sync::Mutex::new(Vec::new()),
        stance: Stance::Gate,
        reason: "no".into(),
    };
    let bound = crate::protocol::agents::BoundAction {
        chain: vec!["child".into(), "grandchild".into()],
        ..bound_leaf("rm -rf ./build")
    };
    let _ = judge(
        &mut frontend,
        &shared,
        &spotter,
        &Toolbox::bash_only(),
        1,
        &approve_call(),
        Some(&bound),
    )
    .await
    .unwrap();

    let seen = spotter.seen.lock().unwrap();
    assert_eq!(seen[0].0, "bash", "the spotter is asked about the leaf");
    assert_eq!(
        seen[0].1,
        vec!["child".to_string(), "grandchild".to_string()],
        "the spotter sees the approval chain"
    );
}

/// The existence check sits ahead of the whole permission cascade: a call naming no
/// advertised tool is answered with the corrective result text without any rung being
/// asked a question it has no way to answer (docs/permissions.md, docs/tools.md).
#[tokio::test]
async fn a_call_naming_no_tool_is_answered_ahead_of_the_cascade() {
    let (shared, _rx) = Session::in_memory();
    let toolbox = Toolbox::bash_only();
    let call = call("ferritin");
    let decision = run_call(
        &mut NeverGates,
        &shared,
        &NeverAsked,
        &toolbox,
        1,
        &call,
        AskPosition { n: 1, of: 1 },
        None,
    )
    .await
    .expect("the check answers rather than erroring");
    let Decision::Direct(payloads) = decision else {
        panic!("an unrunnable call is answered directly, never run or gated");
    };
    let [Payload::ToolResult { call_id, raw, wire }] = &payloads[..] else {
        panic!("exactly one paired tool_result, or projection orphans the tool_use");
    };
    assert_eq!(call_id, &call.id);
    assert!(raw.is_error, "the call was not cogent");
    // `Direct` payloads never enter the triage, so the correction reaches the wire
    // verbatim — no summary stands between it and the model (docs/triage.md).
    assert!(matches!(wire, WireState::Raw));
    for expected in ["unknown tool: ferritin", "Mounted tools: bash", "`bash`"] {
        assert!(
            raw.content.contains(expected),
            "{expected:?} in {:?}",
            raw.content
        );
    }
}

/// An `archive` with no recorded succession behind it is an ordinary gated call: it
/// reaches the spotter and then the gate, as any other session-control call does
/// (docs/permissions.md).
#[tokio::test]
async fn an_archive_without_a_succession_goes_to_the_spotter() {
    let shared = parent_with(crate::config::Config::default());
    let mut frontend = RecordingFrontend::answering(Approval::Abort);
    let spotter = crate::curation::spotter::StaticSpotter {
        stance: Stance::Gate,
        reason: "would gate".into(),
    };
    let call = ToolCall::new(
        crate::protocol::agents::ARCHIVE_WIRE_NAME,
        "toolu_archive",
        serde_json::json!({ "agent": "brisk-raven" }),
    );
    let approval = judge(
        &mut frontend,
        &shared,
        &spotter,
        &Toolbox::bash_only(),
        1,
        &call,
        None,
    )
    .await
    .unwrap();
    assert_eq!(approval, Approval::Abort);
    assert_eq!(
        frontend.decisions.len(),
        1,
        "the spotter's gate reached the human"
    );
}

/// A toolbox with the session-control verbs mounted for `caller`, over a host that holds
/// no sessions.
fn with_agent_verbs(caller: &str) -> Toolbox {
    let host = crate::host::Host::new(
        crate::inference::InferenceRegistry::empty(),
        crate::config::env_table::EnvSnapshot::new(Default::default()).unwrap(),
    );
    let mut toolbox = Toolbox::bash_only();
    toolbox.mount_sessions(crate::clients::control_tools::Control::in_session(
        &host,
        caller.into(),
    ));
    toolbox
}

/// A denial or an escalation authorizes nothing, so it clears on the fast path: neither
/// rung is asked, and nothing is classified as an action it would authorize
/// (docs/permissions.md).
#[tokio::test]
async fn a_deny_or_an_escalate_clears_on_the_fast_path() {
    let shared = parent_with(crate::config::Config::default());
    for call in [
        decide_call("deny"),
        {
            let mut with_message = decide_call("deny");
            with_message.input["message"] = "use the cached copy".into();
            with_message
        },
        decide_call("escalate"),
    ] {
        let approval = judge(
            &mut NeverGates,
            &shared,
            &NeverAsked,
            &with_agent_verbs("parent"),
            1,
            &call,
            None,
        )
        .await
        .unwrap();
        assert_eq!(approval, Approval::Approve, "{:?}", call.input);
    }
    assert!(
        !shared
            .snapshot()
            .iter()
            .any(|e| matches!(&e.payload, Payload::Spot(_))),
        "no spotter verdict was asked for or logged"
    );
    // Without the session-control verbs mounted the name is no denial at all.
    assert!(!Toolbox::bash_only().authorizes_nothing(&decide_call("deny")));
}

/// Only an allow is bound to the move it authorizes: an allow whose target holds no
/// gate is answered before any rung, while a deny is not bound, clears, and runs —
/// where the verb itself refuses the target (docs/permissions.md).
#[tokio::test]
async fn only_an_allow_is_bound() {
    let (shared, _rx) = Session::in_memory();
    let toolbox = with_agent_verbs("parent");
    let run = async |call: &ToolCall| {
        run_call(
            &mut NeverGates,
            &shared,
            &NeverAsked,
            &toolbox,
            1,
            call,
            AskPosition { n: 1, of: 1 },
            None,
        )
        .await
        .expect("answered")
    };
    let Decision::Direct(payloads) = run(&decide_call("allow")).await else {
        panic!("an allow with nothing to bind is answered directly");
    };
    let [Payload::ToolResult { raw, .. }] = &payloads[..] else {
        panic!("one paired result");
    };
    assert!(
        raw.is_error && raw.content.starts_with("not run — "),
        "{}",
        raw.content
    );

    let Decision::Ran { result, .. } = run(&decide_call("deny")).await else {
        panic!("a deny is not bound, so it clears and runs");
    };
    assert!(
        result.is_error,
        "the verb refuses a target the host does not hold"
    );
}
