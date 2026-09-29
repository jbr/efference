//! Tests for the approval cascade, with the test doubles it needs.
use super::*;

/// A frontend that must not be reached. A call naming no tool cannot run, so nothing
/// on the permission path — human gate included — should be asked about it.
struct NeverGates;

impl Frontend for NeverGates {
    async fn read_user_turn(&mut self) -> Option<crate::frontend::SubmittedTurn> {
        unreachable!("the loop is not running")
    }
    async fn approve(
        &mut self,
        call: &ToolCall,
        _bound: Option<&crate::control_tools::BoundAction>,
    ) -> Approval {
        panic!(
            "a call naming no tool reached the human gate: {}",
            call.name
        )
    }
    async fn interrupted(&mut self) -> crate::frontend::Interrupt {
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
    ) -> crate::spotter::Spotting {
        panic!("a call naming no tool reached the spotter: {}", call.name)
    }
}

/// A frontend that records what the gate was asked to decide, for the bound
/// `sessions__approve` ladder tests. It answers with a fixed decision.
struct RecordingFrontend {
    decisions: Vec<(String, Option<crate::control_tools::BoundAction>)>,
    decision: Approval,
}

impl Frontend for RecordingFrontend {
    async fn read_user_turn(&mut self) -> Option<crate::frontend::SubmittedTurn> {
        unreachable!("the loop is not running")
    }
    async fn approve(
        &mut self,
        call: &ToolCall,
        bound: Option<&crate::control_tools::BoundAction>,
    ) -> Approval {
        self.decisions.push((call.name.clone(), bound.cloned()));
        self.decision.clone()
    }
    async fn interrupted(&mut self) -> crate::frontend::Interrupt {
        unreachable!("no tool runs")
    }
}

/// A spotter that records the move it was asked about, so a test can pin that the
/// parent's rung judged the child's leaf rather than the `sessions__approve` call.
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
    ) -> crate::spotter::Spotting {
        self.seen
            .lock()
            .unwrap()
            .push((call.name.clone(), chain.to_vec()));
        crate::spotter::Spotting {
            read: crate::spotter::Read {
                stance: self.stance,
                reason: self.reason.clone(),
            },
            origin: crate::log::Origin::Model,
            axes: None,
            usage: None,
        }
    }
}

/// A session whose effective policy is `role` — the parent in a bound-approve test.
fn parent_with(role: crate::config::Config) -> Arc<Session> {
    let (shared, _rx) = Session::with_role(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
        crate::config::LoadedConfig {
            role,
            source: crate::config::ConfigSource::Default,
        },
    );
    shared
}

/// The `sessions__approve` call the parent's model wrote, naming the child and the
/// held call.
fn approve_call() -> ToolCall {
    ToolCall::new(
        crate::control_tools::APPROVE_WIRE_NAME,
        "toolu_parent",
        serde_json::json!({ "session": "child", "call_id": "toolu_leaf" }),
    )
}

/// A leaf the child holds, bound to the parent's approval.
fn bound_leaf(command: &str) -> crate::control_tools::BoundAction {
    crate::control_tools::BoundAction {
        name: crate::bash::NAME.into(),
        input: serde_json::json!({ "command": command, "intent": "whatever" }),
        intent: Some("whatever".into()),
        cwd: std::env::current_dir().unwrap(),
        session: "child".into(),
        chain: vec!["child".into()],
    }
}

/// A granted `sessions__approve` clears blind (docs/permissions.md): that grant is
/// the config's statement of trust in the coordinator, so the child's leaf is not
/// re-litigated and neither rung is consulted.
#[tokio::test]
async fn a_granted_sessions_approve_clears_blind() {
    let mut role = crate::config::Config::default();
    role.tools
        .insert(crate::control_tools::APPROVE_WIRE_NAME.into());
    let shared = parent_with(role);
    let mut frontend = RecordingFrontend {
        decisions: Vec::new(),
        decision: Approval::Abort,
    };
    let spotter = crate::spotter::StaticSpotter {
        stance: Stance::Gate,
        reason: "would gate".into(),
    };
    let approval = super::step::judge(
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
    let mut frontend = RecordingFrontend {
        decisions: Vec::new(),
        decision: Approval::Abort,
    };
    let spotter = crate::spotter::StaticSpotter {
        stance: Stance::Gate,
        reason: "would gate".into(),
    };
    let approval = super::step::judge(
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
            .contains(crate::control_tools::APPROVE_WIRE_NAME),
        "the approval tool itself was never granted"
    );
}

/// A leaf outside the parent's grants gates, and what reaches the gate is the leaf,
/// carried on the parent's own approval call (docs/permissions.md).
#[tokio::test]
async fn a_leaf_outside_the_grants_gates_with_the_bound_action() {
    let shared = parent_with(crate::config::Config::default());
    let mut frontend = RecordingFrontend {
        decisions: Vec::new(),
        decision: Approval::Approve,
    };
    let spotter = crate::spotter::StaticSpotter {
        stance: Stance::Gate,
        reason: "outside the mission".into(),
    };
    let bound = bound_leaf("rm -rf ./build");
    let approval = super::step::judge(
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
        crate::control_tools::APPROVE_WIRE_NAME,
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
/// `sessions__approve` call (docs/permissions.md).
#[tokio::test]
async fn the_spotter_judges_the_leaf_with_the_chain() {
    let shared = parent_with(crate::config::Config::default());
    let mut frontend = RecordingFrontend {
        decisions: Vec::new(),
        decision: Approval::Approve,
    };
    let spotter = RecordingSpotter {
        seen: std::sync::Mutex::new(Vec::new()),
        stance: Stance::Gate,
        reason: "no".into(),
    };
    let bound = crate::control_tools::BoundAction {
        chain: vec!["child".into(), "grandchild".into()],
        ..bound_leaf("rm -rf ./build")
    };
    let _ = super::step::judge(
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
    let (shared, _rx) = Session::new(
        std::env::current_dir().unwrap(),
        crate::persist::Log::in_memory(),
    );
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
    // `Direct` payloads never enter the triage gate, so the correction reaches the wire
    // verbatim — no summary stands between it and the model (docs/redaction-fork.md).
    assert!(matches!(wire, WireState::Raw));
    for expected in ["unknown tool: ferritin", "Mounted tools: bash", "`bash`"] {
        assert!(
            raw.content.contains(expected),
            "{expected:?} in {:?}",
            raw.content
        );
    }
}
