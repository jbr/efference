# Trajectory consolidation

Consolidation is compaction-as-a-fold-over-a-span: keep the conclusion, drop the
deliberation, pay exactly one deliberate cache write. It generalizes [the redaction
fork](redaction-fork.md) from a single tool result to an entire task trajectory — the
same "flip a `WireState`, never mutate `raw`" discipline applied at span granularity
rather than entry granularity.

## The tool

`consolidate(summary)` is an agent-called tool, not a harness-emitted boundary. The
agent invokes it to fold its own finished work — typically right as it starts a new task
— writing the summary it would want to read at the top of the next task. It is
advertised in `tools[]` alongside the world-facing tools, but it is the first tool that
acts on [the substrate](substrate.md), not the world. The agent loop in `src/agent.rs`
therefore dispatches it **structurally** rather than routing it through the `Toolbox`
(`src/tools.rs`): when the call arrives and the human approves, the loop appends a
`Mark { kind: MarkKind::Boundary { upto }, note: summary }` plus a paired `ToolResult`
("consolidated") to keep the `tool_use` answered. No world-run, no distill.

The tool description deliberately frames the summary as *the* record going forward — no
"you can re-read it later." The raw transcript survives in the log for audit and
reversibility, but that is an operator property, not something the prompt dangles. This
is the "distillate IS the output" stance: telling the model its work is recoverable
manufactures anxiety and causes over-dropping.

## The fold point

`upto` is set to the entry just before the most recent `User` turn. This preserves the
live task instruction — the agent keeps the fresh ask while the prior task's trajectory
folds away. The `extent` function in `src/consolidate.rs` computes this by scanning the
log in reverse for the most recent `User` entry, then taking the maximum `seq` strictly
less than that entry's seq.

## The projection fold

`project` in `src/project.rs` handles consolidation in two passes.

**Pre-pass** (`extract_boundaries`): walk the log and collect every
`Payload::Mark(Mark { kind: MarkKind::Boundary { upto }, note })`. For each, record the
boundary and insert into an elided-seq set: every entry with `seq <= upto` plus the
mark entry itself. When multiple boundaries exist their elided sets union; a later fold's
larger `upto` subsumes the earlier one's span.

**Main pass**: iterate the log in order, maintaining a cursor into the collected
boundaries. Before processing each non-elided entry, advance the cursor past any
boundary whose `upto` is less than the current entry's seq and inject the boundary's
summary as a `user`-role text message. This places the summary at the head of the live
conversation — just before the first entry that follows the fold — rather than at the
mark's own log position. Then skip elided entries and project everything else normally.

The `consolidate` `tool_use` / `tool_result` pair projects normally (it is not in the
elided set). It remains visible on the wire as low-noise context — the resolved call to
keep it was that eliding it saves little while losing the explicit record of the
compaction event.

## Multiple consolidations

Multiple consolidations overlap rather than compose hierarchically. Each later fold
subsumes the earlier ones' spans (its `upto` is larger), and each emits its own summary
injection. The wire reads: summary-A · summary-B · … · live turns. The chain of summaries
is the model's session history. Because each fold rewrites the prefix from its fold
point, each consolidation costs exactly one re-cache of everything after `upto` — the
deliberate cache write. The prefix before `upto` remains stable and continues to be
served from cache.

## Branch-from-seq

Replay-and-branch from any `seq` (running a task again from any prior point to try a
different tool call or prompt) rides the same `Mark`/`seq` substrate. A branch is simply
a replay of `project(log[:seq])` with a divergent tail appended; no additional log
machinery is needed.

## Two senses of "checkpoint"

These stay distinct and must not be conflated:

- **Replay checkpoints**: every turn, free, unlimited. This is what "branching" means —
  the log is append-only and every historical `seq` is permanently addressable.
- **Cache breakpoints**: at most four live `cache_control` markers on the wire at any
  time, placed by `place_trailing_cache_breakpoint` in `src/project.rs`. A placement
  strategy, not an addressing mechanism.

## Why `Commit` is not built

A `Commit` payload that would freeze a boundary — making it immutable at the log level
rather than purely at the projection level — is anticipated in [the substrate](substrate.md)
but not built. It is not needed: the fold is pure projection over `Mark::Boundary`, and
the canonical wire is already lean and append-only behind the breakpoint. The immutability
invariant holds without a separate payload variant.

## Deferred refinements

A forked-compaction peek (producing a candidate summary in a throwaway context before
committing the boundary), forward-context re-read, nesting, a bucket-fill backstop, and
headless auto-approve are tracked in the backlog as follow-on robustness work.
