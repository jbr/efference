//! The in-process executor (docs/tools.md): a session's own toolbox reaching the host
//! it lives in, with the caller's identity, and the target rules that identity makes
//! enforceable.

use std::sync::{Arc, Weak};

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::args;
use crate::daemon::Host;
use crate::protocol::agents::{ARCHIVE_WIRE_NAME, BoundAction, DECIDE_WIRE_NAME, allows};
use crate::protocol::sessions::CreateSession;
use crate::protocol::transcript::Handle;
use crate::record::log::ToolCall;
use crate::session::Session;

/// The in-process executor (docs/tools.md): the calling session's toolbox reaches
/// the host directly — the same `Session` methods the HTTP handlers call, never a
/// round trip over the socket. The target rules live here because this is the only
/// executor that knows who is calling:
///
/// 1. the target is never the caller, so a session cannot resolve its own gate;
/// 2. a driving verb reaches only the sessions the caller drives: its descendants (via
///    `created_by`), the sessions it supervises (via the host's supervisor pointer), and
///    its own predecessor (via the host's succession record), so a session cannot steer
///    a peer it was not given; and
/// 3. `decide` reaches only the sessions the caller supervises, by the same pointer:
///    creating a session or succeeding its creator does not by itself make its gates
///    the caller's (docs/permissions.md). The one exception is a gate escalated to the
///    caller, which it may decide, and only that gate.
///
/// An id reaches its session wherever on the host it is; only the overview, `status`
/// with no agent, is bounded to the caller's project.
#[derive(Clone)]
pub struct HostExecutor {
    /// The host, held weakly (see [`super::Executor::host`]).
    host: Weak<Host>,
    /// The calling session's id — the executor's identity.
    caller: String,
}

impl HostExecutor {
    pub(super) fn new(host: &Arc<Host>, caller: String) -> Self {
        Self {
            host: Arc::downgrade(host),
            caller,
        }
    }

    /// The calling session's id.
    pub(super) fn caller(&self) -> &str {
        &self.caller
    }

    /// The host, or an error if it has shut down.
    pub(super) fn host(&self) -> Result<Arc<Host>> {
        self.host
            .upgrade()
            .ok_or_else(|| anyhow!("the host is no longer running"))
    }

    /// The calling session itself, or an error if it has ended.
    pub(super) fn caller_session(&self) -> Result<Arc<Session>> {
        self.host()?
            .session(&self.caller)
            .ok_or_else(|| anyhow!("the calling session {:?} is no longer hosted", self.caller))
    }

    /// Resolve the target a call names, enforcing rule 1. A call that names no
    /// agent defaults to the caller, which the rule then refuses — which is
    /// what makes an in-session read with no target an error rather than a
    /// silent self-read.
    fn target(&self, requested: Option<&str>) -> Result<String> {
        let target = requested.unwrap_or(&self.caller).to_string();
        if target == self.caller {
            return Err(anyhow!(
                "refused: {target:?} is you — a session cannot address itself; name another \
                 agent, as `agents__status` lists them"
            ));
        }
        Ok(target)
    }

    /// Whether `id` is in the caller's project: its `.efference/` storage base is the
    /// caller's. The overview's bound (docs/tools.md).
    fn in_project(&self, host: &Host, id: &str) -> bool {
        let Ok(caller) = self.caller_session() else {
            return false;
        };
        let base = caller.storage_base().map(std::path::Path::to_path_buf);
        match host.session(id) {
            Some(session) => session.storage_base().map(std::path::Path::to_path_buf) == base,
            None => host.starting(id).is_some_and(|starting| {
                base.as_deref()
                    == Some(
                        starting
                            .project()
                            .join(crate::record::session_dir::SESSION_BASE)
                            .as_path(),
                    )
            }),
        }
    }

    /// The target's `Session`, in whichever project it is (docs/tools.md).
    fn hosted(&self, target: &str) -> Result<Arc<Session>> {
        self.host()?.session(target).ok_or_else(|| {
            anyhow!(
                "no agent {target:?} on this host — `agents__status` lists the ones in this \
                 project"
            )
        })
    }

    /// The `source` the caller's turns, decisions and interrupts carry into `target`:
    /// `session[<id>]`, naming the caller's project as well when it is not `target`'s
    /// (docs/tools.md).
    pub(super) fn source_for(&self, target: &Session) -> String {
        crate::record::names::sources::session_source(
            &self.caller,
            self.place_for(target).as_deref(),
        )
    }

    /// The `source` of a note the caller leaves in `target`'s inbox: its bare id within
    /// the project, as a status notification names its session, and the
    /// [`source_for`](Self::source_for) label from another project (docs/inbox.md).
    pub(super) fn note_source_for(&self, target: &Session) -> String {
        match self.place_for(target) {
            Some(place) => {
                crate::record::names::sources::session_source(&self.caller, Some(&place))
            }
            None => self.caller.clone(),
        }
    }

    /// The caller's project directory, with the home directory as `~`, when it is not
    /// `target`'s project; `None` when they share one.
    fn place_for(&self, target: &Session) -> Option<String> {
        let caller = self.caller_session().ok()?;
        (caller.storage_base() != target.storage_base())
            .then(|| crate::platform::home::tilde(caller.directories().project()))
    }

    /// The target for a read, or for a message from a session that does not drive
    /// it: rule 1.
    pub(super) fn resolve(&self, requested: Option<&str>) -> Result<(Arc<Session>, String)> {
        let target = self.target(requested)?;
        Ok((self.hosted(&target)?, target))
    }

    /// The target for a driving verb: rule 1 and rule 2.
    pub(super) fn resolve_driven(&self, requested: Option<&str>) -> Result<(Arc<Session>, String)> {
        let (session, target) = self.resolve(requested)?;
        if !self.drives(&target)? {
            return Err(anyhow!(
                "refused: you don't drive {target:?} — a driving verb reaches the sessions you \
                 created, supervise, or succeed; `agents__status` and `agents__transcript` still read \
                 it, and `agents__message` leaves it a note"
            ));
        }
        Ok((session, target))
    }

    /// The target for `decide` on the held call `seq`: rule 1 and rule 3.
    /// A refusal says who decides the target's gates and which verbs still reach it.
    pub(super) fn resolve_decided(
        &self,
        requested: Option<&str>,
        seq: &str,
    ) -> Result<(Arc<Session>, String)> {
        let (session, target) = self.resolve(requested)?;
        let host = self.host()?;
        if !Self::may_decide_gate(&host, &self.caller, &target, seq) {
            return Err(anyhow!(not_supervised(
                &target,
                session.supervisor().as_deref(),
                self.drives(&target)?
            )));
        }
        Ok((session, target))
    }

    /// Whether the caller drives `target` (rule 2).
    pub(super) fn drives(&self, target: &str) -> Result<bool> {
        let host = self.host()?;
        Ok(Self::may_drive(&host, &self.caller, target))
    }

    /// Whether the caller supervises `target`, and so may decide its gates (rule 3).
    /// False once the host has shut down.
    pub(super) fn supervises(&self, target: &str) -> bool {
        self.host()
            .is_ok_and(|host| Self::may_decide(&host, &self.caller, target))
    }

    /// Rule 3: whether `decider` may decide `target`'s gates — the host's supervisor
    /// pointer for `target` names it (docs/permissions.md). The gate need not be routed
    /// to it: under a dial that sends the gates to the operator, the supervisor may still
    /// decide them. The operator's own surfaces are not sessions and are not asked.
    fn may_decide(host: &Host, decider: &str, target: &str) -> bool {
        host.supervisor_of(target).as_deref() == Some(decider)
    }

    /// Rule 3 for one held call: `decider` supervises `target`, or `target`'s pending
    /// gate is the call `seq` names and its last escalation went to `decider`
    /// (docs/permissions.md). An escalation grants that gate and no other.
    fn may_decide_gate(host: &Host, decider: &str, target: &str, seq: &str) -> bool {
        Self::may_decide(host, decider, target)
            || host
                .session(target)
                .and_then(|session| session.escalated_to())
                .is_some_and(|(to, handle)| to == decider && names_held(handle.as_deref(), seq))
    }

    /// Whether `target`'s `created_by` chain reaches `ancestor`. The tree is
    /// acyclic by construction; the `seen` set only guards a malformed one.
    fn chain_reaches(host: &Host, ancestor: &str, target: &str) -> bool {
        let mut seen = std::collections::HashSet::new();
        let mut current = target.to_string();
        while seen.insert(current.clone()) {
            match host.created_by(&current) {
                Some(parent) if parent == ancestor => return true,
                Some(parent) => current = parent,
                None => return false,
            }
        }
        false
    }

    /// Rule 2: whether `driver` drives `target` — `target` is `driver`'s descendant,
    /// `driver` is `target`'s supervisor, or `target` is `driver`'s predecessor, which
    /// handed its work to `driver` (docs/tools.md). Supervision is not transitive: it
    /// reaches the supervised session, not the sessions that session created or
    /// supervises.
    fn may_drive(host: &Host, driver: &str, target: &str) -> bool {
        Self::chain_reaches(host, driver, target)
            || host.supervisor_of(target).as_deref() == Some(driver)
            || host.predecessor(driver).as_deref() == Some(target)
    }

    /// The sessions in the caller's project, by id, the caller left out.
    pub(super) fn project_ids(&self) -> Result<Vec<String>> {
        let host = self.host()?;
        Ok(host
            .listing()
            .into_iter()
            .map(|row| row.id)
            .filter(|id| *id != self.caller && self.in_project(&host, id))
            .collect())
    }

    /// Whether `call` is an `archive` of the caller's predecessor, by the host's
    /// succession record. False when the host holds no record — after a host restart the
    /// link is not restored, so nothing clears here then.
    pub(super) fn completes_hand_off(&self, call: &ToolCall) -> bool {
        if call.name != ARCHIVE_WIRE_NAME {
            return false;
        }
        let Some(target) = call.input.get("agent").and_then(Value::as_str) else {
            return false;
        };
        let Ok(host) = self.host() else {
            return false;
        };
        host.predecessor(&self.caller).as_deref() == Some(target)
    }

    /// Bind an allowing `agents__decide` call to the leaf action its target holds
    /// (docs/permissions.md). The first hop obeys the same target rules the verb
    /// enforces when it runs — never self, supervised sessions only.
    /// While the held call is itself an allowing decision, follow its `agent` and `seq`
    /// through the host to the leaf, each hop required to be supervised by the
    /// previous; refuse on a moved gate or a cycle.
    pub(super) fn resolve_bound_action(&self, call: &ToolCall) -> Result<BoundAction> {
        let requested = call.input.get("agent").and_then(Value::as_str);
        let mut expected = Self::seq_of(call)?;
        // The parent's own call: rule 1 and rule 3.
        let (mut session, direct_id) = self.resolve_decided(requested, &expected)?;
        let mut chain = vec![direct_id];
        let mut previous = chain[0].clone();
        let mut seen = std::collections::HashSet::new();
        seen.insert(previous.clone());
        loop {
            let gate = session.pending_gate().ok_or_else(|| {
                anyhow!(
                    "gate moved: {previous} is holding no call for a decision — read its \
                     `status` for what it is doing"
                )
            })?;
            if !names_held(gate.handle.as_deref(), &expected) {
                return Err(anyhow!(
                    "gate moved: {expected} is not the call {previous} is holding{}",
                    gate.handle
                        .as_deref()
                        .map_or_default(|handle| format!("; it holds {handle}"))
                ));
            }
            if !allows(&gate.call) {
                let intent = gate.call.intent().map(str::to_string);
                let top = chain[0].clone();
                return Ok(BoundAction {
                    name: gate.call.name,
                    input: gate.call.input,
                    intent,
                    cwd: gate.directories.working().to_path_buf(),
                    project: gate.directories.project().to_path_buf(),
                    session: top,
                    chain,
                });
            }
            // The held call is itself an allowing decision: follow it to its own target.
            let host = self.host()?;
            let next = gate
                .call
                .input
                .get("agent")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("the held `{DECIDE_WIRE_NAME}` names no agent to follow"))?
                .to_string();
            let next_seq = Self::seq_of(&gate.call)?;
            if !Self::may_decide_gate(&host, &previous, &next, &next_seq) {
                return Err(anyhow!(
                    "refused: {previous:?} does not supervise {next:?}, so the decision it \
                     holds would be refused when it ran — deny it, with a message saying why"
                ));
            }
            if !seen.insert(next.clone()) {
                return Err(anyhow!(
                    "refused: the decision chain revisits {next:?} — a cycle, which no decision can follow to its end; deny \
                     the held decision, with a message saying why"
                ));
            }
            session = self.hosted(&next)?;
            expected = next_seq;
            previous = next.clone();
            chain.push(next);
        }
    }

    /// The handle a decision's input names.
    fn seq_of(call: &ToolCall) -> Result<String> {
        args::handle_in(&call.input, "seq").ok_or_else(|| {
            anyhow!(
                "`decide` takes `seq`: the held call's handle, as the agent's `status` gives it"
            )
        })
    }

    /// `create` in-session: the child records the caller as its creator, and this
    /// waits out the preparation, so the calling agent reads a failure as this call's
    /// error rather than finding the child gone later (docs/tools.md).
    pub(super) async fn create(&self, body: CreateSession) -> Result<(Arc<Session>, String)> {
        let host = self.host()?;
        let lineage = crate::config::env::Lineage {
            created_by: Some(self.caller.clone()),
            succeeds: None,
        };
        let starting = host
            .create(body, lineage)
            .map_err(|reason| anyhow!("{reason}"))?;
        let session = starting
            .finished()
            .await
            .map_err(|reason| anyhow!("{} did not start: {reason}", starting.id()))?;
        Ok((session, starting.id().to_string()))
    }

    /// `hand_off` in-session: the caller's work goes to a new session in its own
    /// directory, which it then succeeds (docs/tools.md).
    pub(super) async fn hand_off(&self, body: CreateSession, brief: String) -> Result<String> {
        let host = self.host()?;
        let handed = host.hand_off(&self.caller, body, brief).await;
        handed.map_err(|reason| anyhow!("{reason}"))
    }
}

/// Why a `decide` of `target` is refused to a session that does not supervise it: who
/// decides its gates — its live `supervisor` and the operator, or the operator alone —
/// and what the caller can still do, which depends on whether it `drives` the target.
fn not_supervised(target: &str, supervisor: Option<&str>, drives: bool) -> String {
    let deciders = match supervisor {
        Some(supervisor) => format!("its supervisor, {supervisor:?}, and the operator decide them"),
        None => "it has no supervisor, so the operator decides them".to_string(),
    };
    let still = match drives {
        true => "agents__message and agents__interrupt still reach it",
        false => "agents__message leaves it a note",
    };
    format!(
        "refused: you don't supervise {target:?}, so its gates are not yours to decide — \
         {deciders}; {still}"
    )
}

/// Whether a decision naming `requested` names the held call whose handle is `held`.
/// A lone call's handle is its bare seq; `seq.1` names it as well.
pub(super) fn names_held(held: Option<&str>, requested: &str) -> bool {
    let (Some(Ok(held)), Ok(requested)) = (held.map(Handle::parse), Handle::parse(requested))
    else {
        return false;
    };
    held.seq == requested.seq && held.call.unwrap_or(1) == requested.call.unwrap_or(1)
}
