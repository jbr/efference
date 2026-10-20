# Supervising agents

You can start other agents on this host and supervise them from inside your session with
the `agents__*` tools. An agent is another efference session, with its own model, context
and turn loop. What you have of one is its status, its transcript, and the notifications
its statuses send you.

## The verbs

- `agents__create` starts an agent (below).
- `agents__status(agent)` says where it stands and names the call that answers it; a read
  ends with its model, context fill, cost, background processes and queued messages. With
  no `agent`: one line per agent in your project, `(yours)` after the ones you drive.
- `agents__transcript(agent)` reads what it said and did.
- `agents__message(agent, text, reply_to?)` sends it a message.
- `agents__decide(agent, seq, decision, message?)` resolves a call it is holding.
- `agents__interrupt(agent)` stops it now: a running tool is killed and it continues from
  that result; a reply being generated ends and it goes idle.
- `agents__archive(agent)` ends it.

(`agents__hand_off` gives your own work to a successor; it is not a supervising verb.)
Every answer is plain text whose first line says what happened; an error says what would
work instead.

## Hearing from your agents

When an agent you created or supervise reaches a decision point, its status arrives in your inbox as a
notification from its id and wakes you: idle, you start a turn on it; mid-turn, you read it
at your next step. A gate that went to the operator arrives without waking you. A newer
status replaces one you have not read. Working is never sent.

To wait for an agent, end your turn. Within a turn, `wait` blocks until the next
notification and returns your inbox.

## Statuses and their moves

A status opens `<id>: <condition>`; every later line opens with a label, and the calls it
names have the id and seq filled in.

- ``working: running bash `cargo test` for 40s`` — nothing to do.
- `waiting for your decision on <call> (seq 8)` — a gate routed to you. `intent:` is what
  the agent said the call is for, `held because:` why its reviewer held it, `decide:` the
  call that answers it.
- `waiting for a decision from the operator or you on <call>` — a gate of an agent you
  supervise that went to the operator. `decide:` says the operator is asked and names the
  call, which you may make as well.
- `waiting for a decision from the operator on <call>` (or from another id) — routed to
  someone else, and not yours to decide; it names no call for you.
- `asking a question (seq 12)` — `question:` is the question, `answer:` the reply that
  answers it.
- `idle, its turn is done (seq 30)` — `said:` is its closing message up to about 4 KB, a
  `cut:` line names the `transcript` read with the rest, and `next:` names the message
  that starts its next turn and the archive that ends it.
- `stopped: <reason>` — its last turn gave up. An empty message retries.
- `paused before re-reading its context uncached (48k tokens)` — its prompt cache expired
  mid-turn. An empty message goes ahead and pays that read.
- `held by its operator`, `blocked: its config file needs the operator` — the operator's
  to clear.
- `ended; its log stays on disk`, `ended with an error: …`.

## Messages, and what driving means

You drive the agents you created and the ones they created, any the operator gave you to
supervise, and a predecessor that handed its work to you. `agents__message` follows that:

- To an agent you drive, it is its next user message: idle, it starts a turn; working, it
  is read after the current step; holding a call, after the decision; held by its
  operator, when the operator releases the hold. A message never resolves a gate. An
  empty `text` continues an agent that stopped or paused.
- To any other agent in your project, it is a note in its inbox, which it may set aside.

The answer says which: `delivered:`, `queued:` and what it waits behind, or
`left in <id>'s inbox`.

## Gates and `decide`

A gate is an agent holding a call that its permission rules did not clear and its reviewer
would not clear either; nothing runs until someone decides. An agent you create sends its
gates to you. Its gates are decided by its supervisor — you, for an agent you create,
until the operator gives it another — and by the operator; for any other agent `decide` is
refused, naming who decides. `agents__decide` names the held call by the seq its status
gives (`8`, or `8.2` when one turn made several calls):

- `allow` runs it. A `message` with it reaches the agent as your message, after the result.
- `deny` with a `message` refuses it, and the agent goes on under your message.
- `deny` alone refuses it and ends the agent's turn.

A seq that is no longer the held call is refused with what the agent holds now
(`gate moved: …`). Only an allow is reviewed as the action it authorizes: your own rules
and reviewer judge the agent's call as if you were making it, so your allow can itself be
held for whoever reviews you. A deny authorizes nothing and is not reviewed.

When you cannot judge a call — it reaches past what you were asked to oversee, or you
cannot tell what it would touch — leave it held and tell the operator which agent holds
which call and why; `ask` puts it to whoever supervises you (the operator, when no agent
does). The agent waits until someone decides.

## Questions

An agent's `ask` reaches you as its `asking` status, and the operator sees it too; the
first answer resolves it. Reply with `agents__message(agent, text, reply_to: "<seq>")`.
While it asks, a message naming no question is refused with the question, and a `reply_to`
naming one no longer pending is refused with who answered it. Several questions from one
turn come one at a time (`question 2 of 3`).

## Reading the transcript

`agents__transcript(agent)` reads the conversation: each message in full under its sender
(`you`, `operator`, or an id), and each call on one line under its seq, with what became
of it:

```
14  bash `cargo test -p core` — run the core tests → allowed by you → ok, 212 lines, it saw 9: two parser failures
```

Every result is filtered before the agent reads it: `it saw 9` means its context holds 9
of the 212 lines, and the text after the colon is the filter's note to it.

With no other argument, it is what is new since your last read; a first read is the last
40 lines. `since: <seq>` reads from a seq on, `tail: <n>` the last n lines. A line is what
is printed under one seq: a message, a call with its outcome, a question with its reply;
log entries that say nothing (telemetry, titles) are not lines. A read never opens on a
result without its call, so it can run a few lines past what you asked for, and the header
says how many lines come before it. `seq: "14"` reads one entry in full: the exact input, the
reviewer's verdict and reason, who decided it, the raw result (up to 16 KiB), and which
lines the agent saw.

An allow you sent a note with reads on the call's line, `→ allowed by you, with "use read
next time"`, rather than as a message of its own.

## Creating and ending agents

`agents__create` takes, among others:

- `message` — the task, written for a reader with none of your context.
- `model` — `<connection>/<model>`; omitted, the model this project has been using.
- `cwd` — relative to yours, defaulting to yours; it has to be in your project (its
  directory or a worktree of it).
- `worktree: true` — its own checkout on branch `efference/<id>` from your commit, so its
  edits stay out of your tree. Without it, it works on your files.
- `autonomous: true` — no gates: a call its rules and reviewer do not clear is declined
  with the reason, and it goes on.

Its gates come to you and its statuses wake you. The operator can take its gates back —
they then go to the operator without waking you, and you may still decide them — or give
it another supervisor, which then decides them instead of you. Its gate statuses name who
decides.

`agents__archive(agent)` ends an agent you drive and every agent it created that was not
`detached`; logs stay on disk. A worktree with no uncommitted changes is removed and its
branch kept unless merged. Agents you create end when you do, unless created `detached`.

## Who sees what

- **It never sees an approval.** An allowed call runs, and its result reads as if nothing
  had held it; neither the gate nor its reviewer's reason reaches its context. So feedback
  names the choice it can see — "use the `read` tool for this" — never an approval or a
  gate it has no record of.
- **It sees a denial, and who denied.** After a deny with a message, the call's result
  reads `Not run — <your id> (agent) replied instead of approving; see the next message.`,
  and your message follows opening `<your id> (agent):`, as your other messages do. After a
  bare deny, the result reads `aborted by user` and its turn ends.
- **It sees autonomous review feedback.** An `autonomous` agent gets each declined call
  back with the reason and a pointer to its config file.
- **You see what it did not.** Its transcript shows each raw result beside how much of it
  the agent saw, and the decision trail: the reviewer's verdict and reason, and who decided.
- **Hearing is not deciding.** You hear every status of an agent you created, a gate
  routed elsewhere included. A gate is yours to decide when its status names a `decide:`
  call: `waiting for your decision` when it was routed to you, `waiting for a decision
  from the operator or you` when the operator was asked and you supervise the agent.
- **It knows who you are.** Its identity line names you as its creator. Its first message,
  your messages and an allow's note open `<your id> (agent):`. A note to an agent you do
  not drive reaches it, if its triage keeps it, as `⟢ <your id>: …`.
- **Reach is the project.** Status, transcript and message reach any agent in your
  project; interrupt and archive only the agents you drive, and decide only the agents you
  supervise. You cannot address yourself, and an agent in another project is not found.

## Cost

Every status, answer and transcript you read stays in your context; read one entry in full
rather than reading long windows. An agent's status footer reports its context and cost.

The host keeps an agent's prompt cache warm for about 30 minutes after its last turn. Past
that, a message, decision or answer that lets it go on is followed by a request that reads
its whole context uncached; a `paused` status gives the size. Archive an agent you are done
with.
