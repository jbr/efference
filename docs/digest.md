# The trajectory digest

A session's log folded to a compact text: one line per tool call, plus the turns the
operator wrote and the replies that ended the agent's turns. It is the substrate for
summarizing a session without reading its whole context — the cold-turn gate's summary
head and the compaction-fidelity evaluation (`eval/compaction/`) both start from it.
`src/digest.rs` implements it; `efference dev digest <session> [--upto <seq>] [--seqs]`
prints it.

## What a line carries

```
### operator
<the turn, verbatim>

• [read src/a.rs, src/b.rs] <the call's intent>
    → <the note triage distilled from its result>
### agent
<the prose of a turn that ended with no call, or that asked a question>
```

- **A call** renders as its tool name, the paths it named, its `intent`
  ([tools.md](tools.md)), and its distilled note ([redaction-fork.md](redaction-fork.md)).
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

Arguments other than paths, result content, mid-turn prose and thinking are left out.
That is what bounds the digest's size by the number of calls rather than the volume of
work they did: a call that read a megabyte contributes one note.

`--upto` folds only entries at or below a seq, so a digest can be cut at any point in a
recorded session. `--seqs` labels each block with its entry's seq, for a reader that
expands entries behind the lines it reads.

## Determinism

The digest reads the log alone and asks no model anything, so the same log and options
yield the same text. It is not on the wire; nothing here constrains the cached prefix.
