//! The `efference mcp` set (docs/operating.md): the agent verbs, served over
//! stdin/stdout, and the server instructions a mounting harness is given. Each module
//! re-exports the shared tool struct.

use super::Control;

mcplease::tools!(
    cli = false,
    Control,
    (Create, create, "create"),
    (Message, message, "message"),
    (Decide, decide, "decide"),
    (Interrupt, interrupt, "interrupt"),
    (Archive, archive, "archive"),
    (Status, status, "status"),
    (Transcript, transcript, "transcript"),
    (Wait, wait, "wait")
);

/// What a mounting harness is told about this server (docs/operating.md): what the
/// tools drive, what driving means, and which call answers each thing an agent can be
/// waiting on.
pub const INSTRUCTIONS: &str = "\
Drive the agents on one efference host. Each agent is a session on this host running \
its own agent loop; its id names both, and every tool but `create` takes it as `agent` \
(a server started with --name drives that session when a call names none). `create` \
starts one in a working directory.

`status` says where an agent stands; with no agent, one line per agent. A status that \
waits on you ends with the exact call that answers it, its seq filled in.

You drive every agent on this host, so a `message` is its next user message: delivered \
at once when it is idle, otherwise queued behind its current step or a held call, and \
the answer says which. An empty message continues an agent that stopped or paused.

A gate is an agent holding a tool call its permission rungs did not clear: nothing runs \
until `decide` names the call's seq with allow (a message rides with its result) or deny \
(with a message the agent goes on under it; without one its turn ends).

A question (its status says asking) is answered by `message` with `reply_to` set to the \
question's seq; a message without it is refused while a question is pending.

`transcript` is what an agent said and did, new since your last read by default; `seq` \
zooms into one entry. `interrupt` stops what it is doing; `archive` ends it and the \
agents it created.

Waiting: `wait` returns the next status you have not seen of the agents you created or \
named with `agent`: a gate, a question, a turn ending, a stop, an end. Claude Code moves \
a long MCP call to a background task and wakes on its result, so a long `timeout_s` \
(600) is safe there; other clients keep it under their own tool timeout.";
