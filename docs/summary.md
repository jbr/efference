# Summaries and compaction

A **summary** is text that replaces a range of the transcript, and the log entry that
carries it (`Payload::Summary`, `src/log/summary.rs`). Two operations write one:

- **Consolidation** — the agent replaces a region of its own transcript with a summary,
  having seen that region in full ([consolidate.md](consolidate.md)). The `consolidate`
  tool and the opportunistic juncture checks make these; `made_by` is `Consolidation`.
- **Compaction** — the harness writes a summary from the trajectory digest
  ([digest.md](digest.md)), without the full transcript, so it can happen when a session
  has gone idle and its cache is gone. `made_by` is `Compaction`, carrying the
  connection, provider and model that wrote it.

The rest of this file is the record, how it projects, and compaction.
`src/compact.rs` writes the summary from the prompt and header in `assets/prompts/`;
`src/digest.rs` supplies what the model reads; `src/daemon/compact.rs` chooses the cut and
appends the entry (`POST /loop/compact`, docs/control-api.md); `src/project.rs` projects it.

## The record

```rust
struct Summary {
    from: Seq, upto: Seq,   // the inclusive range replaced; from == 0 for a head summary
    text: String,
    made_by: MadeBy,        // Consolidation | Compaction(LoggedInference)
    audience: Audience,     // All | Triage
}
```

On disk the fields serialize in that order, and `made_by` and `audience` are omitted at
their defaults (`Consolidation`, `All`). A log written before version 4 carried this as a
`Mark` with a `kind` of `Boundary { upto }` (`from` 0) or `Span { from, upto }`, a `note`,
and a `writer` of `agent` or `narrator`; `scripts/migrate-log-v4.py` converted it
([log-and-projection.md](log-and-projection.md), "The header").

- **`made_by`** records who wrote `text`. Projection reads it to decide the framing below;
  nothing else reads the model.
- **`audience`** names which projection the summary applies to: `All` or `Triage`.

A `Triage` summary exists for a triage model that runs on its own client
(`Triager::own`, [triage.md](triage.md)) with a smaller or earlier-degrading window than
the main model. It replaces the range for that client only. The main projection ignores it
and is byte-identical to the projection of the same log without it; a triage that shares
the agent's client reads the main projection and has no notion of it, since a replacement
there would rewrite the cached prefix the triage exists to share. Which projection a reader
asks for follows from how the triage was constructed, not from the model's identity, so a
session recorded with a separate triage model and later booted with a shared one passes
over its `Triage` summaries. For the triage client both audiences apply, like any other
pair of overlapping summaries (the later `upto` subsumes the earlier range).

`project` takes the reader as an argument beside the image placement; the digest takes it
as an option, so a digest read for the main model does not render a triage-only summary.
The separate triage's text view ([triage.md](triage.md)) leads with the text of every head
summary that applies to the triage reader and leaves out the entries they cover; the
title peek on the own model reads the same view, and the idle triage, sent on the agent's
client, reads the main projection.
Nothing constructs a `Triage` summary yet; when something does, the operator-facing
transcript will need to show one as such, so a triage-only replacement does not read as the
main model having lost context.

## How a summary projects

The summary entry projects to nothing where it was appended. Its range `from..=upto` is
elided and its text is injected before the first retained entry after `upto`, so a
summary adopted while the trajectory kept going lands where its range ends. The text is
framed by `made_by`:

- `Consolidation` text is introduced by `⟢ consolidation: entries #from–#upto were
  replaced by this account of them, written by this session with those entries in view.`
  so it does not read as an operator turn.
- `Compaction` text is projected as written, since the header described below carries the
  explanation.

Summaries a `Resumed` entry rolled back, and summaries for the other reader, are ignored.
Several summaries each keep their own text, in log order, ahead of the entries after the
latest one.

## What a compaction produces

A compaction's text is a header followed by an account.

- **The header** is written by the harness, from `assets/prompts/summary-header.md`. It
  says the earlier conversation up to a point is no longer in view and was replaced because
  the cache had expired, that anything after the account is the record verbatim, what the
  writer was shown (the operator's messages verbatim, the agent's replies where it handed
  back, and for each call its stated purpose and a one-line note on the result; no file
  contents, diffs or command output), and that the workspace is described as it was where
  the account ends. It does not name the writing model. The model never sees it and cannot
  soften it.
- **The account** is written by the model from `assets/prompts/compaction.md`: history in
  phases, in the past tense; then a closing section for the present state (in flight, open
  threads, standing direction with the operator's words quoted where the wording matters,
  decisions and their reasons, what was ruled out, files in play). It marks uncertainty
  rather than resolving it, names a file, function, line or ID only if the digest has it,
  and is addressed to the agent as "you": the same session continuing.

The length target (about 1,000 words) is part of the prompt text, not a request
parameter. The prompt and length are the cheapest variant that rated no differently from
the others, not a tuned optimum: no measure tried separated prompt variants
(journal/2026-10-05-compaction-results.md). The prompt calls the human "the operator", as
the session preamble does.

## Who writes it

The session's own model, through the session's own connection: one request whose single
user turn is the digest followed by the compaction prompt, under the session's system
prompt and `tools[]`, with request origin `compaction`. The user chooses the model by
choosing the session's. There is no second connection to configure and no summarizer tier
in the harness. The account is expected as text; a tool call in its place is an error, as
in the title ask ([session-title.md](session-title.md)).

A request that fails, stops at the output cap, calls a tool or returns no text is an
error, reported as one. Nothing in this path substitutes another summary, another model,
or an unsummarized read: whether to retry, edit or accept the cost of a cold read is the
operator's, and whatever asks for a compaction asks the operator (the cold session's
compaction is the intended caller; backlog/inbox/cold-session.md).

The compaction worker (`daemon/compact.rs`) starts with every session, whatever the
connection's capabilities say about a prompt cache. Only the cache-expiry machinery — the
cache lifetime the cold belief reads, its watcher, the keepalive — is conditional on a known
prompt cache. A route that accepts `warm: true` needs no cold belief, so an accepted
request must always have a worker; a connection whose cache support is unknown can be
caching implicitly, and unknown is not treated as absent.

## The cut

A compaction can leave the most recent payloads on the wire verbatim after the summary
(`upto` sits before them). The model is then given the whole digest with a cut line
(`digest::CUT`) placed before the first block past the cut, and covers only up to it: the
record after the cut follows the summary in full, so the account does not retell it but
covers what it refers back to, and dates "where things stand" to the cut. A cut that would
separate a call from its result is refused before any request
(`consolidate::split_calls`), since a summary there could not project. Choosing the cut
belongs to the caller (backlog/inbox/cold-session.md).

## A session compacted more than once

The projection keeps every summary's text, in order, ahead of the entries after the latest
one, so a later compaction adds a summary rather than replacing the earlier ones. Its model
is given the earlier head summaries first, under `compact::EARLIER` and without their
headers, as what the reader already has, then a digest of only the entries past the latest
head summary's `upto`. It covers from where the last summary stops and does not retell it:
a summary of a summary would compound the losses of both.

## Running it by hand

`efference dev compact <session> [--upto <seq>] [--cut <seq>]` runs the compaction over a
recorded log and prints the text, with timing and usage on stderr. `--upto` treats the log
as ending there; `--cut` ends the summary early, as a compaction with a tail would. The
model is the one the log recorded at boot, resolved through the current config, under the
recorded system prompt. It appends nothing, since a session's log has one writer, the host.

## While the summary is written

`compact::summarize` hands the response's fragments to a callback as they arrive. The
compaction routes the text and thinking to the session's `output` broadcast as
`compaction_delta` and `compaction_thinking_delta` (docs/control-api.md), distinct from the
main loop's `AssistantText` and `ThinkingText` and exempt from the rule that a committed
`Assistant` entry retires block deltas. They are not logged and not projected: the
summary entry stays the only record. `efference dev compact` passes a callback that
ignores them.

## What the log records

A compaction is three kinds of entry, all projecting to nothing but the summary's text. An
accepted `POST /loop/compact` appends `CompactionRequested`, carrying the `warm` flag and
the cut, and the compaction is written at that cut. Its request's `RequestStage` entries
follow under the `compaction` origin. It ends with the compaction's `Summary`, or with a
`CompactionFailed` carrying the error (a superseding turn is one). The compaction's state
— waiting for its request, writing, or failed — is folded from these with the open
requests (`daemon/operations.rs`) rather than held in memory, so a failure is still
reported after a host restart; a compaction open when the host stopped is not, since the
process writing it is gone. A failure stands until a turn's request goes out or another
compaction is asked for. On `/state` (docs/control-api.md), an open compaction —
waiting or writing, with its `warm` flag, cut, and the instant its state began — is one of
`current_operations`, since it runs beside the loop and overlaps the idle phase, a cold
park, and a held pace; a failure is not in flight and is its own topic,
`compaction_failure`. Whether `POST /loop/compact` would accept now is the `verbs`
topic's `compact` entry, read from the route's own check (`Session::compaction_verdict`).

## What is not here

When a compaction is offered, and the clients' controls for it, belong to the cold session
(backlog/inbox/cold-session.md).
