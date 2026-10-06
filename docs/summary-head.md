# The summary head

A session's earlier trajectory replaced by an account of it, written by a model from the
trajectory digest ([digest.md](digest.md)) and spliced in as a [`Mark::Boundary`
](consolidate.md) note. It is the second producer of that mark. The first, `consolidate`,
is the agent writing its own summary mid-session; here the harness does the writing, so it
can happen when the session has gone idle and its cache is gone. `src/narrate.rs` makes
the head, from the prompt and header in `assets/prompts/`; `src/digest.rs` supplies what
the narrator reads; `src/daemon/fold.rs` chooses the cut and appends the mark
(`POST /loop/fold`, docs/control-api.md); `src/project.rs` folds it in.

## What is produced

A head is a header followed by an account.

- **The header** is written by the harness, from `assets/prompts/summary-header.md`. It
  says the earlier conversation up to a point is no longer in view and was replaced because
  the cache had expired, that anything after the account is the record verbatim, what the
  writer was shown (the operator's messages verbatim, the agent's replies where it handed
  back, and for each call its stated purpose and a one-line note on the result; no file
  contents, diffs or command output), and that the workspace is described as it was where
  the account ends. It does not name the writing model. The narrator never sees it and
  cannot soften it.
- **The account** is written by the narrator from `assets/prompts/narrator.md`: history in
  phases, in the past tense; then a closing section for the present state (in flight, open
  threads, standing direction with the operator's words quoted where the wording matters,
  decisions and their reasons, what was ruled out, files in play). It marks uncertainty
  rather than resolving it, names a file, function, line or ID only if the digest has it,
  and is addressed to the agent as "you": the same session continuing.

The narrator's length target (about 1,000 words) is part of the prompt text, not a request
parameter. The prompt and length are the cheapest variant that rated no differently from
the others, not a tuned optimum: no measure tried separated prompt variants
(journal/2026-10-05-compaction-results.md). The prompt calls the human "the operator", as
the session preamble does.

## Who writes it

The session's own model, through the session's own connection: one request whose single
user turn is the digest followed by the narrator prompt, under the session's system prompt
and `tools[]`. The user chooses the model by choosing the session's. There is no second
connection to configure and no summarizer tier in the harness. The account is expected as
text; a tool call in its place is an error, as in the title peek ([session-title.md](session-title.md)).

A request that fails, stops at the output cap, calls a tool or returns no text is an
error, reported as one. Nothing in
this path substitutes another summary, another model, or an unsummarized read: whether to
retry, edit or accept the cost of a cold read is the operator's, and whatever asks for the
head asks the operator (the cold session's fold, not yet built, is the intended caller; backlog/cold-session.md).

## The cut

A fold can leave the most recent payloads on the wire verbatim after the head (the
mark's `upto` sits before them). The narrator is then given the whole digest with a cut
line (`digest::CUT`) placed before the first block past the cut, and narrates only up to
it: the record after the cut follows the account in full, so the account does not retell
it but covers what it refers back to, and dates "where things stand" to the cut. A cut
that would separate a call from its result is refused before any request
(`consolidate::split_calls`), since a boundary there could not project. Choosing the cut
belongs to the caller (backlog/cold-session.md).

## The mark

A `Mark` gains two fields, both defaulted so existing logs deserialize unchanged:

- **`writer`** — who wrote the note: `Agent` (a `consolidate` call, the default for every
  mark already in a log) or `Narrator(model)` with the connection, provider and model that
  wrote it, in the same log-owned shape the boot preamble uses. Nothing reads the model
  yet; it is a record of which model wrote the note.
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
Nothing constructs a `Triage` mark yet; when something does, the operator-facing transcript
will need to show one as such, so a triage-only fold does not read as the main model having
lost context.

## Running it by hand

`efference dev narrate <session> [--upto <seq>] [--cut <seq>]` runs the narrator over a
recorded log and prints the head, with timing and usage on stderr. `--upto` treats the log
as ending there; `--cut` ends the account early, as a fold with a tail would. The model is
the one the log recorded at boot, resolved through the current config, under the recorded
system prompt. It appends nothing, since a session's log has one writer, the host.

## What is not here

When a fold is offered, and the clients' controls for it, belong to the cold session
(backlog/cold-session.md).
