# The summary head

A session's earlier trajectory replaced by an account of it, written by a model from the
trajectory digest ([digest.md](digest.md)) and spliced in as a [`Mark::Boundary`
](consolidate.md) note. It is the second producer of that mark. The first, `consolidate`,
is the agent writing its own summary mid-session; here the harness does the writing, so it
can happen when the session has gone idle and its cache is gone. `src/narrate.rs` makes the
account; `src/digest.rs` supplies what it reads; `src/project.rs` folds it in.

## What is produced

A head is a header followed by an account.


- **The header** is written by the harness, from `assets/prompts/summary-header.md`. It
  says the earlier conversation is no longer in view and was replaced because the cache had
  expired, names the model that wrote the account, says what that model was shown (the
  operator's messages verbatim, the agent's replies where it handed back, and for each call
  its stated purpose and a one-line note on the result; no file contents, diffs or command
  output), and says the workspace is described as it was when the session went idle. It
  carries one wording whether or not the writer is the model reading it. The narrator
  never sees it and cannot soften it.
- **The account** is written by the narrator from `assets/prompts/narrator.md`: history in
  phases, in the past tense; then a closing section for the present state (in flight, open
  threads, standing direction with the operator's words quoted where the wording matters,
  decisions and their reasons, what was ruled out, files in play). It marks uncertainty
  rather than resolving it, names a file, function, line or ID only if the digest has it,
  and is addressed to the agent as "you": the same session continuing.

The narrator's length target (about 1,000 words) is part of the prompt text, not a request
parameter. The prompt calls the human "the operator", as the session preamble does.

## Who writes it

The session's own model, through the session's own connection: one request, the narrator
prompt and the digest, no tools. The user chooses the model by choosing the session's.
There is no second connection to configure and no summarizer tier in the harness.

A request that fails or returns nothing usable is an error, reported as one. Nothing in
this path substitutes another summary, another model, or an unsummarized read: whether to
retry, edit or accept the cost of a cold read is the operator's, and whatever asks for the
head asks the operator (the cold-turn gate, not yet built, is the intended caller).

## The mark

A `Mark` gains two fields, both defaulted so existing logs deserialize unchanged:

- **`writer`** — who wrote the note: `Agent` (a `consolidate` call, the default for every
  mark already in a log), `Digest` (the raw digest as the head, no model involved), or
  `Narrator(model)` with the connection, provider and model that wrote it, in the same
  log-owned shape the boot preamble uses. The header's model name comes from here.
- **`audience`** — which projection the mark applies to: `All` (the default) or `Triage`.

A `Triage` boundary exists for a triage model that runs on its own client
(`ForkPeek::own`, [redaction-fork.md](redaction-fork.md)) with a smaller or earlier-degrading
window than the main model. It folds the span for that client only. The main projection
ignores it and is byte-identical to the projection of the same log without it; a peek that
shares the agent's client reads the main projection and has no notion of it, since a fold
there would rewrite the cached prefix the peek exists to share. Which projection a reader
asks for follows from how the peek was constructed, not from the model's identity, so a
session recorded with a separate triage model and later booted with a shared one simply
passes over its `Triage` marks. For the triage client both audiences apply, folding like
any other pair of consolidations (the later `upto` subsumes the earlier span).

`project` takes the reader as an argument beside the image placement; the digest takes it
as an option, so a digest read for the main model does not render a triage-only summary.
The operator-facing transcript shows a `Triage` mark as such.

## What is not here

The trigger (when a head is offered), the operator's accept and edit steps, and the host
route that appends the mark belong to the cold-turn gate. `efference dev narrate
<session> [--upto <seq>]` runs the narrator step over a recorded log and prints the head;
it appends nothing, since a session's log has one writer, the host.
