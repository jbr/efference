# The redaction fork — triage on every tool result

Every tool result enters the canonical lineage through a fork: a throwaway branch
that shares a cached prefix with canonical but whose raw content and extraction
prompt are never appended to it.

## The gate covers a completion's results, not one result

A gate is opened once per completion and carries every result that completion produced.
Usually that is one, since every request asks for a single call
([README.md](README.md)) — but the ask is not enforceable, so the loop runs all of a
completion's calls first and opens the gate over the batch.

This grouping is load-bearing, not a convenience. The peek's prefix ends with the
assistant message, which carries one `tool_use` per call; the API rejects a request that
leaves any of them unanswered. A peek fired *between* calls therefore builds an invalid
request whenever a completion made more than one — the later calls have no results yet.
Running every call before opening the gate means the tail can answer all of them with
real output, so nothing is orphaned and no placeholder is needed.

Each result is rendered as its own block, headed by its `[call id]` and **numbering from
1**. A single continuous coordinate system across results would invite the model to keep
lines from one result under another's verdict — a mis-attribution that renders exactly
like a correct verdict, so nothing downstream could detect it. The `retain` verdict is
correspondingly an array keyed by `call_id`; a verdict naming an id that was never
offered is dropped, the same harness-side guard applied to notification ids and roster
names.

The inbox, background tails, and surfacing roster are **gate-scoped** — decided once per
step rather than per result — so they ride the trailing text block and their dispositions
land on the last `Triage` of the batch. That placement is tail-ward, keeping the wire
append-only, and is identical to where they land when a completion carried one call.

```
[cached prefix through tool_use]  ──┬──►  + [full raw result] + [retain prompt]   (throwaway: 1 turn, tail uncached)
                                    └──►  + [distilled result]                     (canonical: appended, cached)
```

The fork's prefix hits cache; only its tail — raw output plus the extraction
instruction — is fresh. The canonical prefix grows monotonically and is never
invalidated. What would otherwise be a mutation problem becomes an append problem.

## Universal, with no small-result exemption

The fork is not an optimisation applied selectively above a threshold. Every tool
result is peeked once, and only the distillate is appended. Peeking a small result
is cheap, and uniformity removes a special case. The payoff is **window economy**
and cache-chain longevity: the canonical context stays maximally lean. The cost is
one extra round-trip per result — latency, not tokens — explicitly accepted. The
point is agent-facing: the agent never has to fear reading a whole file, because
only the relevant parts come back (see [positions.md](positions.md)).

## The `retain` verdict

The fork forces a single structured tool call — no freeform parsing. The verdict
is one of three forms:

| form | meaning |
| --- | --- |
| `retain(all)` | every line survives |
| `retain(lines=[40-52, 310-318])` | these 1-based inclusive ranges survive |
| `retain(none, note="…")` | nothing survives; the one-line note is the breadcrumb |

`retain(none)` is not silent: the note stays on the canonical wire so the agent
doesn't re-run what it just disowned — inline dead-end shedding at the source.

On any failure (the peek errors, or calls something other than `retain`), the
gate falls back to keep-all. Data is never lost — but this is a failure, not a neutral
default. `WireState` is fixed once, so an uncurated result stays in the cached prefix for
the rest of the session and every later turn pays for it. The fallback is therefore
logged at error, and the `Triage` records a **breadcrumb** in `raw_keep` describing what
came back: the model's unparseable `retain` input, the tool it called instead, or the
transport error. Without that, a malformed verdict, a wrong-tool call and a 429 are
indistinguishable in the log after the fact.

Surfacing an exhausted failure to the operator as a session-visible error — rather than
only a log line — is deferred to the turn-failure work, which needs the same mechanism
for upstream request failures.

## Line numbers are the coordinate system

All tool output is line-numbered before it is shown in the fork peek. Line
numbers are the coordinate system `retain(lines=…)` addresses, and
the distillate preserves original numbers across elisions so any kept line maps
back to the raw. The raw in the log stays pristine; numbering is a render transform
applied only inside the throwaway branch.

## Two renderings: the fork view and the canonical wire

Line-per-line numbering inflates the canonical wire — a keep-all verdict on a
small result would be larger than the raw it replaces. The two renderings are
therefore split:

- **`number_lines`** (`cat -n` per line) — shown only in the throwaway fork peek;
  the coordinate system `retain(lines=…)` addresses. Implemented in `src/distill.rs`.

- **`render_hunks`** (the canonical wire form) — a `@@ lines a-b @@` header per
  contiguous kept span, then those lines verbatim. One header per range (not per
  line); gaps between hunks are implied by the jump in headers, as in a unified
  diff. A keep-all result is one header plus verbatim body, approximately equal to
  raw. `render_hunks` runs at **projection time** over the stored verdict
    (`WireState::Distilled { keep, note }`), not at append — the wire form is
  re-derived from `raw` plus the verdict each turn, so the kept bytes are never
  stored twice. When a note survives, `render_hunks` prefixes it with the `⟢`
  provenance sigil — the same marker surfaced-context lines carry (`⟢ context ·
  …`) — so the note is honestly signed as harness-curated output without invoking
  mechanism vocabulary. The sigil is render-layer only; the fork produces a bare
  `note` string and is unaware of how it's rendered. Implemented in `src/project.rs`.

## The verdict is the stored form

`WireState::Distilled` carries the **verdict** (the `keep` ranges and optional
`note`), not the rendered bytes. Two payoffs from one move: the kept bytes are
never persisted twice, and the structured decision is itself the record.

The fork's bulky transcript — the full line-numbered output plus the `retain`
instruction — stays throwaway and never enters canonical. That is what protects
caching. But the fork's *decision* is logged as a `Triage` entry (in `src/log.rs`):

| field | meaning |
| --- | --- |
| `for_result` | `seq` of the `ToolResult` this verdict distilled (`None` for a standalone idle triage) |
| `origin` | `Model` (a real `retain` call) or `Fallback` (keep-all; the peek failed or a model-free stub ran) |
| `raw_keep` | the model's exact `keep` string, pre-parse — the audit breadcrumb |
| `inbox` | per-notification dispositions chosen in this peek, by arrival `seq` |

`origin` carries the one signal the rendered artifact erases: a deliberate
keep-all and a failed peek render identically, so only `Triage.origin` distinguishes
them. `project` reads `Triage` only as audit (it emits pinned-notification blocks
from `inbox`) and skips it otherwise. Every fork decision is reconstructable from
the log — including ones only ever visible inside the throwaway branch — for
observability and distiller-quality work, with none of the fork's context touching
the wire or the cache.

## The fork manages perception, not the world

Redaction is one instance of a more general role: the fork is where the agent
decides **what it will perceive next** — which lines of a result survive, which
notifications to pin or dismiss (see [the inbox doc](inbox.md)), which streams to
subscribe to or tail. All of it is a reversible projection choice over the agent's
own context; none of it acts irreversibly on the world, so none of it needs the
approval gate, and all of it is safe to settle off-main.

The fork's invariant holds throughout: prefix cached, tail discarded, except an
explicit structured result appended to canonical (the `Distilled` verdict, a pinned
`Ambient`). The line that bounds this: the moment the fork would do something
irreversible to the world (`kill -9` a process), that is a main-branch, gated
action, not perception management.

## Surfacing unloaded context — the relevance rider

Deciding what to perceive next also runs *forward*: the same peek can pull an
**unloaded** `[context]` item (a read-on-demand skill/memory stub, see
[context.md](context.md)) into view when it bears on the work in flight. A cheap
model that recalls a stub's description but answers from memory anyway is missing a
*decision* step, not a fact; the rider lands the nudge adjacent to the moment of
need. Implemented in `src/distill.rs` (the roster + verdict) and `src/project.rs`
(the render).

- **Roster.** The peek view lists the surfaceable items — every `Item::Listed`
  (those with a `path`) in the boot listing that no prior gate has surfaced — by
  `[name]`, and `retain` gains an optional `surfaced` field over those names. The
  roster is a pure fold over the prefix; names double as the constrained output
  vocabulary, validated against the roster (invented names dropped).
- **Surfaced-once dedup.** The fold excludes any name already in a prior
  `Triage.surfaced`, so an item is offered at most once per session — the same
  shape as the inbox's `active_shown`, `Triage`-only, no path-load tracking. It
  suppresses on *surfaced*, not *read*; closing that gap is the pin-until-read
  escalation, which is the only part that would need path tracking.
- **Verdict is the stored form.** The chosen names ride on `Triage.surfaced`
  (`src/log.rs`); `project` re-renders each as a `⟢ context · <name> …: <desc> —
  read <path>` reminder **at the Triage position**, folding name→(path,
  description) from the `Preamble`'s structured listing snapshot — tail-ward, so the
  wire stays append-only, exactly like a pinned notification. Nothing but the names
  is stored; the render evolves without touching stored entries.

The rider is a **late-session** mechanism: its value is surfacing a relevant item
the agent has drifted away from, which a constrained single turn can't manufacture
(a capable model self-serves before the rider's window opens). So its causal payoff
is a dogfooding question, not a single-turn test — while its cheap complement,
stronger stub descriptions, works on its own at boot.

## `reread` — agent-initiated re-peek, tail-additive

A `reread(seq, focus?)` tool re-runs the existing `ForkPeek` on the raw still in
the log and **appends** the new, fuller distillate at the tail — pure append,
prefix untouched, cache-clean. The same call serves "more of X" and "all of it
now" (`retain(all)`). The content reappearing at the tail is correct: the agent
re-read it, mirroring re-opening a file.

The naive alternative — flipping a committed `WireState` back to `Raw` or a fuller
distillate — mutates a cached prefix entry and invalidates everything downstream
(see [log-and-projection.md](log-and-projection.md) on cache-prefix immutability), so it is
rejected. `reread` reuses the universal fork verbatim; only
the trigger differs (agent-initiated vs. at-append).

Building `reread` is gated on evidence of over-redaction causing a wrong or
incomplete answer; tracked in the backlog.

## Block addressing — the visible `seq` handle

Each tool result carries its `seq` as a visible handle on the wire: `tool_result`
content is prefixed `[<seq>] …` (e.g. `[2] file.txt`), a render-time tag requiring
no schema change. This is what makes "reread 14" / "branch from 14" / "compare to
block 14" describable across the model wire, the TUI, and the control plane.

The **stable `seq`** is used, not a positional wire-index. A positional count
renumbers on elision or consolidation, which would bust the cache. `seq` is
immutable and survives compaction; it is a property of the log, and `/log`
plus `/events` already serialize it. A `BlockId` alias can later hide the
representation (`seq`, session-prefixed `seq`, short-sha) if needed. Tagging every
entry uniformly (vs. only tool results) is tracked in the backlog.
