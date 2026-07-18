# efference — architecture

> An experimental, API-backed agentic coding harness in Rust, built to make
> *context management* a first-class, iterable concern rather than an afterthought.
> The name is from the neuroscience term *efference copy*: the copy of a motor
> command an organism keeps to predict and reconcile the sensory consequences of its
> own action. A harness that deliberately curates what it will "perceive" next is
> doing the same thing.

This is the top of the architecture tree. It states the thesis, the one load-bearing
seam, and the invariants everything bends around — then points at the per-subsystem
docs, each of which names the module that implements it so you can go straight to the
code instead of grepping. The docs are present-tense and describe how the system works
now. Planned work lives in [`../backlog/`](../backlog/); the terse record of shipped
features is [`../CHANGELOG.md`](../CHANGELOG.md); session narrative memory is
[`../journal/`](../journal/).

## The thesis

Most of an agent's perceived quality is context-curation quality, and most harnesses
treat "the context" as a mutable message list they append to and occasionally
summarize. efference rejects that. The central move:

**The context is a deterministic projection over an append-only annotated log.**

- The **log** is the source of truth: append-only, ordered, durable to disk. It holds
  everything ever seen — full tool results, ambient events, and salience marks
  (decisions, dead-ends, fold-points) recorded *at the moment they're known*.
- The **wire transcript** sent to the API is `project(log)` — a materialized view,
  recomputed each turn.
- Redaction never mutates stored output. It flips a per-entry `WireState`
  (`Raw → Distilled → Elided`). Because the raw result survives in the log, every
  redaction is reversible: re-project with a lighter hand, or re-read what we
  *concluded* versus what's *there now* — distinct operations.

Four behaviors the design wanted collapse into this one data model, each becoming a
change to the **projection function** rather than a rewrite of the core:

| Desired behavior | Becomes |
| --- | --- |
| Intent-aware compaction (drop dead-end deliberation, keep the conclusion) | a projection that keeps `Mark`s and elides the spans they summarize |
| Continuous scrubbing (drop the irrelevant lines of a result) | a projection that `Elide`s or `Distill`s those entries |
| Progressive doc/tool reveal | a projection where a subtree / tool-def hasn't been expanded yet |
| Reversibility of all of the above | raw output is never overwritten |

The same move appears at every layer: the model's context is `project(log) → messages`;
the human's TUI transcript is a *second* `project(log) → renderable tree`; a reconnecting
control-channel client catches up by replaying the log. One substrate, many views.

## The seam

The whole system rides on a clean split between the **substrate** (the log) and the
**view** (the projection), meeting the **transport** at one seam:

```rust
struct LogEntry { seq: u64, turn: u32, at: Timestamp, payload: Payload }

enum Payload {
    User(Vec<ContentBlock>),
    Assistant { text: Vec<ContentBlock>, calls: Vec<ToolCall> },
    ToolResult { call_id: CallId, raw: RawResult, wire: WireState },
    Mark(Mark),         // a salience annotation (e.g. a consolidation fold-point)
    Ambient(Notification),  // an out-of-band notification
    Triage(Triage),     // a fork verdict — the audit record
}

enum WireState {
    Raw,                                             // project the full raw result
    Distilled { keep: Keep, note: Option<String> },  // a verdict, re-rendered by project
    Elided,                                          // project a placeholder; still recoverable
}

fn project(log: &[LogEntry]) -> Vec<ApiMessage>;     // deterministic, byte-stable
```

`raw` is immutable. Redaction only flips `wire`, and `wire` carries the *verdict*
(`keep` ranges + an optional note), not rendered bytes — `project` re-renders from `raw`
each turn, so the wire is genuinely `project(log)` recomputed, never a string lodged in
the substrate. See [the substrate](substrate.md) for the full type set and
[the redaction fork](redaction-fork.md) for how verdicts are produced.

## The invariants (do not break these)

These are the constraints the whole design bends around. Violating any one silently
breaks caching or correctness:

1. **`project` is byte-deterministic.** Identical log → byte-identical wire, every time.
   Prompt caching is a prefix match: any byte change in the prefix invalidates everything
   after it. A non-stable projection silently kills the cache. (Determinism hazards:
   serialize tool-call inputs with sorted keys; never project timestamps.) Two tests pin
   this — re-projecting an identical log yields identical bytes, and the exact wire
   encoding is fixed so any reorder/rename of a wire field fails *on purpose*.
2. **Committed regions are immutable on the wire; all mutation happens in the uncommitted
   tail past the last cache breakpoint.** Redaction is fixed *at append*, so no cached
   prefix entry is ever mutated. The one operation that rewrites prefix — a consolidation
   fold — is exactly one deliberate cache write.
3. **raw is never overwritten; only `wire` flips.** This is what makes every redaction
   reversible and the log a complete audit record.
4. **One tool call per assistant turn.** The request forces at most one `tool_use` per
   turn, so the redaction fork is always single-result, the approval gate is
   one-command-per-turn, and the log stays strictly alternating `U → A·t → R → …`.
5. **No privileged client.** The loop's only input is the HTTP control channel; the human
   TUI, the driver, and subagents are all equal clients of the same socket. See
   [the control channel](control-channel.md).

## Shared vocabulary

To describe wire/log states compactly, two aligned rows — the **LOG** (source of truth;
everything, raw) and the **WIRE** (`project(LOG)`; what is sent):

| token | meaning |
| --- | --- |
| `U` | user turn |
| `A` | assistant prose |
| `t(cmd)` | `tool_use` — a tool call |
| `R(…)` | a **raw** tool result (full content) — lives in the LOG only |
| `r(…)` | a **distilled** tool result — what the WIRE carries, e.g. `r(L40-52)` |
| `┃` | a cache breakpoint |
| `_` | an elided entry (projects to a placeholder) |

Example — a big `cat` distilled to two line ranges, a small `grep` kept whole, breakpoint
at the tail:

```
LOG :  U  A t(cat file)  R(486 lines)       A t(grep fork)  R(7 lines)
WIRE:  U  A t(cat file)  r(L40-52,310-318)   A t(grep fork)  r(L1-7)     ┃
```

## The map

Read top-down; open the subsystem doc you need.

| Subsystem | Doc | Governing modules |
| --- | --- | --- |
| The log, the projection, the caching constraint | [substrate.md](substrate.md) | `src/log.rs`, `src/project.rs`, `src/persist.rs` |
| The universal triage gate — how every tool result is distilled | [redaction-fork.md](redaction-fork.md) | `src/distill.rs`, `src/project.rs` |
| The tool seam (in-process `Tool` trait; in-process MCP) | [tools.md](tools.md) | `src/tools.rs`, `src/bash.rs` |
| Trajectory consolidation (compaction as a fold over a span) | [consolidate.md](consolidate.md) | `src/consolidate.rs`, `src/agent.rs` |
| Ambient notifications — the inbox (enqueue then triage) | [inbox.md](inbox.md) | `src/inbox.rs`, `src/daemon.rs` |
| Background processes — the supervisor | [supervisor.md](supervisor.md) | `src/supervisor.rs`, `src/bash.rs` |
| Control channel & the `Frontend` trait (HTTP over a Unix socket) | [control-channel.md](control-channel.md) | `src/daemon.rs`, `src/client.rs`, `src/frontend.rs`, `src/agent.rs` |
| Permissions — role config & the approval fast-path | [permissions.md](permissions.md) | `src/config.rs`, `src/policy.rs`, `src/agent.rs` |
| The `[context]` preamble builder — what the session boots knowing | [context.md](context.md) | `src/context.rs`, `src/config.rs`, `src/env.rs`, `src/policy.rs` |
| Transport & streaming (upstream SSE, deltas, keepalive) | [transport.md](transport.md) | `src/api.rs` |
| The TUI client | [tui.md](tui.md) | `src/tui/` |

The turn engine that ties it together is `src/agent.rs`; the daemon CLI entry point is
`src/main.rs`.

## Conventions for this tree

- **Present tense.** Describe what *is*, not what landed when. The dated narrative lives
  in [`../journal/`](../journal/); the shipped-feature list in [`../CHANGELOG.md`](../CHANGELOG.md).
- **No file:line citations** — they rot. Name the governing module inline (e.g.
  "implemented in `src/project.rs`") so a reader knows where to look, and let the code
  carry the line-level detail.
- **Depth bar:** a sufficiently sophisticated peer should be able to write a rough
  approximation of the project from these docs alone. The key types, the control/projection
  flow, the invariants, and the load-bearing *why* belong here; exact details are the
  code's job.
- **Rationale is load-bearing only** — kept when it protects an invariant a future reader
  could unknowingly break, or records a road already taken and rejected. No decisions log.
- A doc graduates from `topic.md` to `topic/README.md` + child files only when it grows too
  big to scan in one sitting.
