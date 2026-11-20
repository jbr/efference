//! The circumstances a spotter judges a move in (docs/permissions.md).

use crate::protocol::agents::Supervision;
use crate::record::log::ToolCall;
use crate::record::utterances::Utterance;
use crate::tools::ToolBrief;

/// Everything [`super::Spotter::assess`] is given beyond the config and the call
/// itself. [`Circumstances::default`] is the plain move: a bash command or
/// structured call with no brief, no approval chain, no probe, no precedents, no
/// known place among the host's sessions, and no known utterances.
/// The `with_*` methods set one input each. The fields borrow from the toolbox
/// and the session, so a value lives for one `assess` call.
#[derive(Default)]
pub struct Circumstances<'a> {
    /// What the mounted tool table records about a structured call's tool — its
    /// description, its input schema, and the server's own behavior hints
    /// (docs/permissions.md). `None` for bash (judged as command text) and when no
    /// toolbox is at hand (the `efference dev spotter` dev command).
    pub brief: Option<ToolBrief<'a>>,
    /// Non-empty when the call is a leaf bound to a parent's `agents__decide`
    /// (docs/permissions.md): the session ids from the directly-targeted child down
    /// to the session holding the leaf. The move presented is the child's action,
    /// and the chain names the approvers it passed through. Empty for an ordinary
    /// move.
    pub chain: &'a [String],
    /// A command prefix picked by the prefix-promotion descent (docs/permissions.md)
    /// to be judged *for promotion* alongside this move: when `Some`, the move prompt
    /// includes the question whether that prefix is safe for **every** argument, and a
    /// [`Stance::ClearAlways`](crate::record::log::Stance::ClearAlways) is the
    /// safe-vote. `None` on most calls.
    pub probe: Option<&'a str>,
    /// The earlier calls in this session that ran with the operator's consent
    /// (`precedent::consented`), oldest first. Empty for a bound leaf, whose history
    /// is the child's, not this session's. The decision rung weighs them; the prose
    /// rung ignores them.
    pub precedents: &'a [ToolCall],
    /// The calling session's place in the supervision relation: the session that
    /// supervises it and the sessions it supervises (docs/permissions.md). For a bound
    /// leaf it is the approving session's, not the leaf holder's. The loop passes it only
    /// for a session-control move (an `agents__*` verb); on any other move it is `None`.
    /// `None` also when it is not known — no host is at hand (the `efference dev
    /// spotter` and `spotter-request` dev commands, the offline eval) — which is not the
    /// same as a session with no supervisor and none supervised. The prose rung renders
    /// it; the decision rung ignores it.
    pub supervision: Option<&'a Supervision>,
    /// What the operator, and any session supervising this one, said in the session,
    /// oldest first (docs/permissions.md). For a bound leaf they are the approving
    /// session's. `None` when they are not known — no session log is at hand (the
    /// `efference dev spotter` dev command, an offline eval case with no readable log) —
    /// which is not the same as a session where nobody has spoken yet. The prose rung
    /// renders them; the decision rung ignores them.
    pub utterances: Option<&'a [Utterance]>,
}

impl<'a> Circumstances<'a> {
    /// The tool brief for a structured call.
    pub fn with_brief(mut self, brief: Option<ToolBrief<'a>>) -> Self {
        self.brief = brief;
        self
    }

    /// The approval chain of a bound leaf.
    pub fn with_chain(mut self, chain: &'a [String]) -> Self {
        self.chain = chain;
        self
    }

    /// The prefix a promotion probe is about.
    pub fn with_probe(mut self, probe: Option<&'a str>) -> Self {
        self.probe = probe;
        self
    }

    /// The consented earlier calls of the session.
    pub fn with_precedents(mut self, precedents: &'a [ToolCall]) -> Self {
        self.precedents = precedents;
        self
    }

    /// The session's place in the supervision relation, when it is known.
    pub fn with_supervision(mut self, supervision: Option<&'a Supervision>) -> Self {
        self.supervision = supervision;
        self
    }

    /// What was said in the session, when it is known.
    pub fn with_utterances(mut self, utterances: Option<&'a [Utterance]>) -> Self {
        self.utterances = utterances;
        self
    }
}
