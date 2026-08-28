# efference — architecture

> An experimental, API-backed agentic coding harness in Rust, built to make
> *context management* a first-class, iterable concern.
> The name is from the neuroscience term *efference copy*: the copy of a motor
> command an organism keeps to predict and reconcile the sensory consequences of its
> own action. A harness that deliberately curates what it will "perceive" next is
> doing the same thing.

This is the top of the architecture tree: what efference is, the core data model,
the invariants, and a map to the per-subsystem docs — each of which names the
module that implements it so you can go straight to the code instead of grepping.
The docs are present-tense and describe how the system works now. One of them is not
architecture: [operating.md](operating.md) is the *usage* document — how to drive a
running session from the outside — and it is also compiled into the binary and served to
sessions through the guide overlay ([context.md](context.md)). The design
positions behind it are in [positions.md](positions.md); planned work lives in
[`../backlog/`](../backlog/); the terse record of shipped features is
[`../CHANGELOG.md`](../CHANGELOG.md); session narrative memory is
[`../journal/`](../journal/).

## What this is

efference is a daemon (`efference serve`) that runs the agent turn loop against an
Anthropic-Messages-compatible endpoint, driven entirely through an HTTP control
channel that the TUI, the CLI verbs, scripts, and other agents share as equal
clients. It is one take on the harness category — a small, deliberately curated
featureset — built on positions rather than a thesis: chiefly that autonomy is a
runtime dial rather than a product identity, and that friction experienced by the
agent is a defect even when the work completes anyway. [positions.md](positions.md)
records the positions and the observed failures behind them.

## The data model

Each session is an append-only log; the wire transcript sent to the API is
`project(log)`, recomputed each turn. Storing a log and rebuilding the request
from it is how most harnesses work; what efference commits to on top of that is:

- **Rendering decisions are stored as per-entry verdicts, not content edits.**
  Redaction flips a `WireState` (`Raw → Distilled → Elided`) carrying the
  *verdict* (`keep` ranges + an optional note); `project` re-renders from the
  immutable `raw` each turn. Because the raw result survives in the log, every
  redaction is reversible and auditable.
- **`project` is byte-deterministic, and tests pin it** (see the invariants
  below).
- **Decisions get first-class log entries** — salience marks, triage verdicts,
  spotter verdicts, config edits — recorded at the moment they're known.

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

Because rendering decisions live in `project`, these behaviors are projection
changes rather than log rewrites:

| Behavior | Becomes |
| --- | --- |
| Intent-aware compaction (drop dead-end deliberation, keep the conclusion) | a projection that keeps `Mark`s and elides the spans they summarize |
| Continuous scrubbing (drop the irrelevant lines of a result) | a projection that `Elide`s or `Distill`s those entries |
| Progressive doc/tool reveal | a projection where a subtree / tool-def hasn't been expanded yet |
| Reversibility of all of the above | raw output is never overwritten |

The log projects to more than one view: the model's context
(`project(log) → messages`), the human's TUI transcript (a second projection of
the same log to a renderable tree, [tui.md](tui.md)), and a reconnecting
control-channel client's catch-up replay ([control-channel.md](control-channel.md)).
See [log-and-projection.md](log-and-projection.md) for the full type set and
[redaction-fork.md](redaction-fork.md) for how verdicts are produced.

## The invariants (do not break these)

Violating any one silently breaks caching or correctness:

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
4. **One tool call per assistant turn — requested, not guaranteed.** Every request sets
   `disable_parallel_tool_use`, so the usual shape is one `tool_use` per turn: the
   approval gate is one-command-per-turn and the log reads `U → A·t → R → …`. But the
   flag is a *request*. A provider that doesn't implement it returns several calls and
   nothing reports that it was ignored — on OpenRouter-routed models the knob is absent
   from every serving endpoint's supported parameters, in either API dialect, so there
   is no provider-side way to enforce it. The loop therefore handles N ≥ 1: it runs every
   call a completion made, then triages their results at one gate
   ([redaction-fork.md](redaction-fork.md)). What *is* invariant is the pairing — every
   `tool_use` gets exactly one `tool_result`, or projection orphans it and the API
   rejects the request.
5. **No privileged client.** The loop's only input is the HTTP control channel; the human
   TUI, the driver, and subagents are all equal clients of the same socket. See
   [control-channel.md](control-channel.md).

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
| The log, the projection, the caching constraint | [log-and-projection.md](log-and-projection.md) | `src/log.rs`, `src/project.rs`, `src/persist.rs` |
| Per-result triage — how every tool result is distilled | [redaction-fork.md](redaction-fork.md) | `src/distill.rs`, `src/project.rs` |
| Tools (in-process `Tool` trait; in-process MCP) | [tools.md](tools.md) | `src/tools.rs`, `src/bash.rs` |
| Trajectory consolidation (compaction as a fold over a span) | [consolidate.md](consolidate.md) | `src/consolidate.rs`, `src/agent.rs` |
| Ambient notifications — the inbox (enqueue then triage) | [inbox.md](inbox.md) | `src/inbox.rs`, `src/daemon/notifications.rs` |
| Background processes — the supervisor | [supervisor.md](supervisor.md) | `src/supervisor.rs`, `src/bash.rs`, `src/daemon/background.rs` |
| Control channel & the `Frontend` trait (HTTP over a Unix socket) | [control-channel.md](control-channel.md) | `src/daemon.rs` (+ `src/daemon/`), `src/client.rs`, `src/frontend.rs`, `src/agent.rs` |
| Permissions — role config & the approval fast-path | [permissions.md](permissions.md) | `src/config.rs`, `src/policy.rs`, `src/agent.rs`, `src/daemon/config_edits.rs` |
| The `[context]` preamble builder — what the session boots knowing | [context.md](context.md) | `src/context.rs`, `src/config.rs`, `src/env.rs`, `src/policy.rs` |
| Operating a session — what an operator can see, change, and say (usage, not architecture) | [operating.md](operating.md) | `src/client.rs`, `src/bin/efference/verbs.rs`, `src/guide.rs` |
| The `[env]` table — stripping and injecting env vars at spawn, masking them at append, resolving vault references | [env.md](env.md) | `src/env_table.rs`, `src/env_table/resolve.rs`, `src/persist.rs`, `src/bash.rs` |
| Transport & streaming (upstream SSE, deltas, keepalive) | [transport.md](transport.md) | `src/api.rs` |
| MCP authorization — the OAuth dance for connected HTTP providers | [mcp-auth.md](mcp-auth.md) | `src/oauth.rs` (+ `src/oauth/`), `src/www_authenticate.rs`, `src/mcp/http.rs` |
| The TCP bind — the web client's front door (root-derived credentials, TOTP bootstrap, the gate) | [web-bind.md](web-bind.md) | `src/auth.rs`, `src/daemon/tcp.rs` |
| The web client | [web-client.md](web-client.md) | `web/`, `src/daemon/tcp.rs` |
| The TUI client | [tui.md](tui.md) | `src/tui/` |

Where a module has grown a directory beside it (`src/api/`, `src/config/`, `src/policy/`,
`src/project/`, `src/distill/`, `src/context/`, `src/daemon/`, `src/tui/*/`), the `.rs` file
holds the subsystem's vocabulary and public surface and the directory holds one file per
concern; each module's tests live in its own `test.rs`.

The turn engine that ties it together is `src/agent.rs`; the CLI binary is
`src/bin/efference/`, whose `serve` verb boots the daemon.

`src/daemon.rs` holds the shared-state vocabulary and the loop wiring; each concern it
serves lives in a `src/daemon/` submodule named for its governing doc — `http.rs` and
`sse.rs` (the control channel), `config_edits.rs` (permissions), `notifications.rs` (the
inbox), `background.rs` (the supervisor), plus `pace.rs`, `approvals.rs`, `metrics.rs`,
`keepalive.rs`, `watch.rs`, and `views.rs` (the `/state` and `/log` readouts).

## Repository layout

Everything is one crate in one repository — daemon, clients, CLI, and the four records.
A workspace split waits for a build target that cannot link the full dependency set; none
currently exists, because rendering for thin clients is daemon-side (the human-wire render
document) rather than a shared client library. Two roads considered and not taken: a
browser-side wasm core (superseded by daemon-side rendering) and multiple repositories
(the control API is unversioned and co-evolves with its clients, wire-format changes want
atomic commits alongside the byte-stability pin, and the records in `docs/`, `backlog/`,
`CHANGELOG.md`, and `journal/` are repo-scoped). Client code in other languages lives
in-tree and is embedded in the serving binary.

## Conventions for this tree

- **Present tense.** Describe what *is*, not what landed when. The dated narrative lives
  in [`../journal/`](../journal/); the shipped-feature list in [`../CHANGELOG.md`](../CHANGELOG.md).
- **No file:line citations** — they rot. Name the governing module inline (e.g.
  "implemented in `src/project.rs`") so a reader knows where to look, and let the code
  carry the line-level detail.
- **Depth bar:** a sufficiently sophisticated peer should be able to write a rough
  approximation of the project from these docs alone. The key types, the
  control/projection flow, the invariants, and the why that protects them belong here;
  exact details are the code's job.
- **Keep rationale only where it earns its place** — when it protects an invariant a
  future reader could unknowingly break, or records a road already taken and rejected.
  No decisions log.
- **Describe, don't evaluate.** State mechanisms and what breaks if they change — no
  praise words, no banner theses, no comparative claims about other harnesses. Assume
  any sentence here may be quoted out of context by a future summarizer; keep only
  sentences that stay true and modest when repeated. These docs are institutional
  knowledge whose primary readers are models, and framing is memetic in both
  directions — rigor propagates as readily as slogans ([positions.md](positions.md)).
- A doc graduates from `topic.md` to `topic/README.md` + child files only when it grows too
  big to scan in one sitting.
