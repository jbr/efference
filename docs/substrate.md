# The substrate — log, projection, caching

The whole system rides on a clean split between **substrate** (the log) and
**view** (the projection). The log is the source of truth; the wire transcript
is `project(log)`, recomputed each turn. Everything else is a consequence of
keeping that seam sharp.

---

## Load-bearing types

### `LogEntry`

```rust
struct LogEntry { seq: u64, turn: u32, at: Timestamp, payload: Payload }
```

`seq` is the monotone address space `project` reads over. `turn` is log
metadata — which agent turn produced the entry. `at` is a wall-clock
timestamp, also metadata only: timestamps are **never projected**, so their
non-determinism cannot touch the wire and cannot break caching.

### `Payload`

```rust
enum Payload {
    User(Vec<ContentBlock>),
    Assistant { thinking: Vec<ContentBlock>, text: Vec<ContentBlock>, calls: Vec<ToolCall> },
    ToolResult { call_id: CallId, raw: RawResult, wire: WireState },
    Mark(Mark),        // a salience annotation
    Ambient(Notification),  // an out-of-band notification (see inbox.md)
    Triage(Triage),    // a fork verdict — the audit record (see redaction-fork.md)
    Spot(Spot),        // a spotter permission verdict — audit only (see permissions.md)
    ConfigEdit(ConfigEdit),  // one durable policy change — folded by the classifier (see permissions.md)
    Preamble { system: String },  // the boot preamble snapshot — replay/audit only (see context.md)
    // Anticipated, not yet built: a frozen cache-breakpoint boundary (`Commit`)
    // for compaction. Kept here as a forward marker — "wire the seam ahead of
    // the feature" — not present in the code today.
}
```

`Commit` is a load-bearing placeholder. When compaction needs an explicit
freeze primitive it slots in here without touching any other type.

`Preamble` records the full rendered `system` string once at boot, exactly as
this session's requests carry it. It exists because the `[context]` preamble
builder ([context.md](context.md)) folds file *contents* into the system
prompt, and those files drift on disk between boots — so the preamble is no
longer reconstructable from `config + current files`, and faithful replay
requires storing it. It projects to nothing (the system prompt rides the
request's `system` field, never `messages`), so appending it cannot perturb
the projected wire; the `wire` dev view reads it back instead of re-resolving
files from disk.

### `WireState` / `Keep`

```rust
enum WireState {
    Raw,                                              // project the full `raw`
    Distilled { keep: Keep, note: Option<String> },  // the verdict, re-rendered by project
    Elided,                                           // project a placeholder
}
enum Keep { All, None, Ranges(Vec<(usize, usize)>) }
```

`WireState` is the one redaction primitive. It lives next to an immutable
`raw: RawResult` on every `ToolResult` entry. **Redaction only ever flips
`wire`; `raw` is never overwritten.** That is what makes every redaction
reversible: the full result survives in the log regardless of what the wire
shows.

Crucially, `Distilled` stores the **verdict** — which line ranges survive
(`keep`) and an optional breadcrumb (`note`) — not the rendered bytes.
`project` re-renders the kept hunks from `raw` each turn, so the wire is a
genuine pure projection, and the kept bytes are never persisted twice. The
structured decision doubles as the audit record. See [the redaction
fork](redaction-fork.md) for how verdicts are produced.

**Note on `Elided`:** `WireState::Elided` is never constructed at runtime. The
variant exists as a defensive arm in projection match statements (see
[project.rs](../src/project.rs)) to handle future redaction modes gracefully, but
current Phase 0 never produces it. Phase 1 (the actual redaction implementation)
will add the construction sites.

### `Mark` / `MarkKind`

```rust
struct Mark { kind: MarkKind, note: String }
enum MarkKind {
    Boundary { upto: Seq },  // compaction fold-point — the only kind built
    // decision / dead-end / relevance kinds land when they get a construction site
}
```

A `Mark::Boundary` entry drives compaction entirely inside `project`:
projection folds every entry with `seq <= upto` to a summary user message.
There is no separate mutation of stored entries; the fold is a projection
concern, not a log mutation. See [consolidate.md](consolidate.md).

### `project`

```rust
fn project(log: &[LogEntry]) -> Vec<ApiMessage>
```

Implemented in `src/project.rs`. A pure deterministic fold over the log that
produces the Anthropic `messages` array. It coalesces consecutive `ToolResult`
entries into one user message — and a `User` entry that directly follows them
merges into that *same* message as `[tool_result…, text]` rather than forming a
second consecutive user-role message, so role-alternation and byte-stability
hold when a user turn is appended right after a turn's results (the seam-drain,
backlog/participation/README.md — the mid-trajectory injection and
the redirect steer both rely on this shape). It omits `is_error: false` for a
lean wire and re-renders `Distilled` wire states from their `keep` + `raw` pair.
A pre-pass over `Mark::Boundary` entries drives the compaction fold; no external
parameter controls it.

---

## Why byte-determinism is a caching correctness property

Prompt caching is a **prefix match**: any byte change anywhere in the prefix
invalidates every cached block after it. Render order is `tools → system →
messages`.

If `project` is not byte-stable — identical log, identical output, every
time — the cache silently breaks. Two guards in `src/project.rs` enforce this
and must not be weakened:

- `projection_is_byte_stable` — re-projecting an identical log yields
  identical bytes.
- `projection_matches_expected_wire_bytes` — pins the exact wire encoding. Any
  reorder, rename, or addition of a wire field fails this test on purpose:
  that change would silently bust the production cache, so it must break a
  test first.

The primary determinism hazard: `HashMap` JSON key order is per-process
random. Tool-call `input` is a `serde_json::Value` specifically because it
serializes object maps with sorted keys. A `HashMap` here would kill caching
silently. When adding wire-facing fields, keep them deterministic and update
the expected-bytes pin deliberately.

---

## Verified API facts (Anthropic)

| Fact | Value | Note |
| --- | --- | --- |
| `cache_control` breakpoints / request | **4 max** | place deliberately; walk them forward as the session grows |
| Prefix lookback | **20 content blocks** | a breakpoint only finds a cache entry within the last 20 blocks; long agentic turns blow past this — drop intermediate breakpoints every ~15 blocks |
| Min cacheable prefix (**Opus 4.8**) | **4096 tokens** | ⚠️ *not* the ~1024 the origin chat assumed (that's Sonnet-4.5-era). Sub-4096 prefixes **silently** don't cache on Opus 4.8 (Fable 5 / Sonnet 4.6 = 2048) |
| Cache write cost | **1.25× (5m TTL), 2× (1h TTL)** | base input price |
| Cache read cost | **0.1×** | the lever |
| Break-even | 2 requests (5m), 3 requests (1h) | |
| Invalidation tiers | tools / system / messages | changing `tool_choice`, toggling `thinking`, adding images invalidate *system+messages* but **not** the tools cache; changing a tool def or the model invalidates everything |
| Concurrent timing | a cache entry is readable only **after the first response begins streaming** | N parallel identical-prefix requests all pay full price — fire one, await first token, then fan out |
| Determinism hazard | `HashMap` JSON key order is per-process random | serialize tool-use inputs via `BTreeMap` or `serde_json` `preserve_order`; a non-stable projection silently kills caching |
| Mid-conversation system msgs | **Opus 4.8**: append `{role:"system"}` to `messages[]` | does **not** invalidate the cached prefix; a non-spoofable operator channel — the natural merge point for ambient/operator events |

> **TTL is a write-time property.** A warm 5m cache cannot be read-upgraded to
> 1h — `ttl` attaches only to `cache_creation` tokens, so a 1h lifetime must
> be chosen at a cold write. Keeping the cache warm across idle (a
> `max_tokens:0` keepalive read that refreshes the sliding TTL while generating
> nothing) and making cache usage legible are transport concerns realized in
> [`src/api.rs`](transport.md) — never log or projection changes, so they
> cannot perturb the byte-stability pins.

---

## The invariant tying caching to redaction

**Committed regions are immutable on the wire; all mutation happens in the
uncommitted tail past the last cache breakpoint.** Three operations must honor
this:

- **Redaction is at append.** Each tool result's `wire` is fixed the moment it
  enters the log and never changes — so no cached prefix entry is ever
  mutated. The invariant holds by construction, the strongest possible form.

- **Compaction folds a span.** Projection over a `Mark::Boundary` elides
  entries with `seq <= upto` and injects a summary, rewriting the prefix from
  the fold point. This is exactly one deliberate cache write; everything past
  the boundary is unaffected.

- **Ambient notifications** never splice behind a breakpoint. They queue
  out-of-band and are logged only at first-show. A pinned notification projects
  at the position of the `Triage` that pinned it — always tail-ward — so the
  wire grows only at the tail, never behind a cached block. Arrival entries
  (`Payload::Ambient`) project to nothing on the canonical wire. See
  [inbox.md](inbox.md).

Because redaction-at-append never touches a committed entry, no explicit freeze
primitive is needed yet. A `Commit` payload that freezes a boundary is
anticipated (see `Payload` above) but not built. Keeping the canonical lineage
lean — distilled, not raw — is what keeps consecutive breakpoints inside the
20-block window longer. Continuous compaction and cache longevity are the same
lever pulled once.

---

## The wire is a pure projection; materializing it is pure memoization

`project` is a deterministic fold, so an in-memory materialized wire is a memo
with no semantic effect — drop it and rebuild byte-identically from the log.
Correctness can never depend on the memo.

Its invalidation boundary coincides with the prompt-cache breakpoint: every
tail append extends the wire only at the tail, and the one event that rewrites
the prefix — a `Mark::Boundary` fold — is exactly the "one deliberate cache
write." Incremental re-derivation falls out of the same prefix/tail seam the
design already bends around.

The memo is available as a pure-efficiency lever but is **never persisted** — a
persisted wire would become a rival source of truth. Tracked in the backlog
until the transcript is long enough to measure.
