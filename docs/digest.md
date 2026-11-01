# The trajectory digest

A session's log folded to a compact text: one line per tool call, plus the turns the
operator wrote and the agent's prose. It is the substrate for
summarizing a session without reading its whole context — the cold session's compaction
and the compaction-fidelity evaluation (`eval/compaction/`) both start from it.
`src/curation/digest.rs` implements it; `efference dev digest <session> [--upto <seq>] [--seqs]`
prints it. A separate triage model reads it as the older part of its text view
([triage.md](triage.md)); the digest at a triage extends the digest at every earlier
triage byte for byte, which is what lets that request resume a cached prefix.

## What a line carries

```
### operator
<the turn, verbatim>

• [read src/a.rs, src/b.rs] <the call's intent>
    → <the note triage wrote for its result>
### agent
<the prose of any turn, including one that goes on to make calls>
```

- **A call** renders as its tool name, the paths it named, its `intent`
  ([tools.md](tools.md)), and its triaged note ([triage.md](triage.md)).
  Paths are read from the per-tool key the permission classifier reads (`path_spec`),
  so a tool with no path key renders its name alone. A result that failed is marked
  `(error)`; one with no note says so.
- **A `Pending` result** takes the wire state its later `ResultVerdict` gave it.
- **An `ask`** renders as the question, and its reply as a `<who> replied` block
  attributed like a turn (`operator`, or the replying agent's source). An ask that got
  no reply renders `(not answered)`, since an open question is an open thread.
- **A consolidation mark** renders its summary in place.
- **Entries a resumption skipped** ([resumption.md](resumption.md)) are omitted, as
  are record-only assistant entries, and every entry that projects to nothing.

- **The agent's prose** renders for every turn that has any, mid-turn narration
  included: it is where the agent says what it took from the operator and what it is
  about to do, which the call lines do not carry. Measured 2026-10-10, it adds little:
  ~4k tokens on a ~32k-token digest for a 250-call gpt-6.1-sol session, under 2k for
  Claude sessions of similar length.

Arguments other than paths, result content and thinking are left out. That is what
bounds the digest's size by the number of calls rather than the volume of work they
did: a call that read a megabyte contributes one note.

**The verbatim form** (the `verbatim` option, no flag) renders each call as its name
and arguments, then its result as the agent read it — the kept lines and note
(`project::result_content`), indented under the call — with operator turns, prose and
asks as above. The text view's triaged tail is this form ([triage.md](triage.md)).

`--upto` folds only entries at or below a seq, so a digest can be cut at any point in a
recorded session. `--seqs` labels each block with its entry's seq, for a reader that
expands entries behind the lines it reads. The `cut` option (no flag; compaction sets it)
places a marker line before the first block past a seq, for a summary that ends there
while the record after it stays verbatim ([summary.md](summary.md)).

## Rendering in spans

`digest::Fold` reads a log once — the entries a resumption set aside, and each held
result's verdict — and renders any span of it. A call renders in the span that holds its
assistant entry, so spans that each begin at an assistant entry concatenate to the whole
digest; `Fold::hide` leaves further entries out of every span (the text view hides what
a head summary covers). `render` is one span over the log.

## Determinism

The digest reads the log alone and asks no model anything, so the same log and options
yield the same text. It is not on the agent's wire. The separate triage's text view sends
it ([triage.md](triage.md)), where what matters is that a later triage's digest extends an
earlier one's; `src/curation/triage/text_view/test.rs` pins that.
