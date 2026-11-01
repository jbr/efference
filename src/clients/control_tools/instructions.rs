//! What a harness mounting `efference mcp` is told about the server (docs/operating.md),
//! in its `initialize` answer.

/// What a mounting harness is told about this server (docs/operating.md): what the
/// tools drive, what driving means, and which call answers each thing an agent can be
/// waiting on.
pub const MCP_INSTRUCTIONS: &str = "\
Drive the agents on one efference host. Each agent is a session on this host running \
its own agent loop; its id names both. `create` starts one in a working directory; \
every other tool takes the id as `agent`, which only `status` and `wait` leave out.

`status` says where an agent stands; with no agent, one line per agent. A status that \
waits on you ends with the exact call that answers it, its seq filled in.

You drive every agent on this host, so a `message` is its next user message: delivered \
at once when it is idle, otherwise queued behind its current step or a held call, and \
the answer says which. An empty message continues an agent that stopped or paused.

A gate is an agent holding a tool call its permission rungs did not clear: nothing runs \
until `decide` names the call's seq with allow (a message rides with its result) or deny \
(with a message the agent goes on under it; without one its turn ends). Escalate leaves \
it held and asks the operator, the person at efference's own clients, with your message \
as a note to them. The agent \
never sees an approval, only a denial and its message, so feedback names the visible tool \
choice, not the gate.

A question (its status says asking) is answered by `message` with `reply_to` set to the \
question's seq; a message without it is refused while a question is pending.

`transcript` is what an agent said and did, new since your last read by default; `seq` \
reads one entry in full. `interrupt` stops what it is doing; `archive` ends it and the \
agents it created.

Waiting: in Claude Code, open a Monitor on the `follow:` URL that `create` or \
`status(follow: true)` gives you; each decision point (a gate, a question, a turn ending, \
a stop, an end) arrives as a notification. `wait` is the path for other clients: the next \
status you have not seen of the agents you created or named with `agent`; keep its \
`timeout_s` under your tool timeout.";
