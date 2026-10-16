# The trajectory digest

A session's log folded to a compact text: one line per tool call, plus the turns the
operator wrote and the agent's prose. It is the substrate for
summarizing a session without reading its whole context — the cold session's fold summary
head and the compaction-fidelity evaluation (`eval/compaction/`) both start from it.
`src/digest.rs` implements it; `efference dev digest <session> [--upto <seq>] [--seqs]`
prints it. `efference dev triage --view digest` replays a session's triages with the
history rendered as this digest instead of the transcript (an experiment arm,
`src/triage/digest_view.rs`); the digest at a triage extends the digest at every earlier
triage byte for byte, which is what would let such a request resume a cached prefix.

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

`--upto` folds only entries at or below a seq, so a digest can be cut at any point in a
recorded session. `--seqs` labels each block with its entry's seq, for a reader that
expands entries behind the lines it reads. The `cut` option (no flag; the narrator sets it)
places a marker line before the first block past a seq, for a summary that ends there
while the record after it stays verbatim ([summary-head.md](summary-head.md)).

## Determinism

The digest reads the log alone and asks no model anything, so the same log and options
yield the same text. It is not on the wire; nothing here constrains the cached prefix.
