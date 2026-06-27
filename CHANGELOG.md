# efference — build journal

The dated, session-by-session record of what was built and verified, with the live
results and the bugs root-caused along the way. This is **history**; for how the
system works *now*, read `DESIGN.md` (the forward-looking spec). Entries are
chronological (oldest first). Section numbers like §3, §10.1 refer to `DESIGN.md`.

---

## 2026-06-23 — Phase 0: transport, substrate, projection, seam

First implementation session. Built and verified:

- **Transport** (`src/api.rs`): non-streaming POST to `/v1/messages` via
  `trillium-client` + `trillium-rustls` (TLS; ALPN negotiates HTTP/2). Two live
  round-trips confirmed against Haiku; `usage` (incl. `cache_*`) parsed.
  `api::Client::complete(&[ApiMessage]) -> Completion`.
- **Substrate** (`src/log.rs`): the minimal walking set — `LogEntry`, `Payload`
  (`User` / `Assistant` / `ToolResult` only), `WireState` (all three), `ContentBlock::
  Text`, `ToolCall`, `RawResult`, `Timestamp`/`Seq`/`Turn`. No `Mark`/`Ambient`/
  `Commit` yet.
- **Projection** (`src/project.rs`): `project(&[LogEntry]) -> Vec<ApiMessage>`.
  Coalesces consecutive `tool_result`s into one user message; omits `is_error:false`.
  Tested for byte-stability **and** an exact expected-wire-bytes pin (the
  cache-determinism guard, §3).
- **Seam closed** (`src/main.rs`): request built from `project(log)`; response parsed
  back into `Payload::Assistant` and appended. Single turn — no loop yet.
- 7 tests green; clean build (edition 2024, nightly). lib + bin split.

Decisions this session (beyond §8): dev model `claude-haiku-4-5` for cost (Opus default
unchanged). **HTTP/3 deferred** — Anthropic is fronted by Cloudflare, negotiates HTTP/2,
advertises no `alt-svc: h3`; an `--http3-only` probe didn't connect. `serde_json::Value`
(sorted-key serialization) gives projection determinism — no `preserve_order` needed.

### Bash tool, Frontend seam, agent loop, persistence, cache breakpoint

- **Bash tool** (`src/bash.rs`) — spawns `bash -c` into a fresh process group
  (`process_group(0)`; child pid == pgid), captures combined stdout/stderr, marks
  `is_error` on non-zero exit, and races completion against an injected `abort` future
  — on abort it `killpg`s the whole group with `SIGKILL` (§6 ownership, seeded now).
  `abort` is injected (not `ctrl_c` inline) so the kill path is unit-tested without
  signalling the runner. 5 tests. Deps: `libc`, tokio `process`.
- **`Frontend` seam** (`src/frontend.rs`) — trait (`read_user_turn` / `approve` /
  `interrupted`) + the `Interactive` backend. A background thread owns the blocking
  stdin read and forwards over an mpsc channel; `approve()` races stdin + SIGUSR1
  (approve) + SIGINT (abort). `Approval` is three-valued — `Approve | Abort |
  Redirect(String)`: an empty line approves, other text redirects, C-c bails. 5 tests.
- **Agent loop** (`src/agent.rs`) — `project → complete → append assistant → {approve,
  run, append result} → repeat` until the model stops calling tools, then
  `read_user_turn` (EOF quits). First turn seeded from **argv**.
- **JSONL persistence** (`src/persist.rs`) — `Log { entries, sink }` appends one
  `LogEntry` per line, flushed per append; `load()` replays for the daemon-attach
  future. Headline test: a log written to disk and read back **projects to
  byte-identical wire**. 18 tests.
- **Trailing cache breakpoint** (`project::place_trailing_cache_breakpoint`) —
  `CacheControl::Ephemeral` on the last block of the last message, applied *after*
  `project` (which stays pure/byte-stable).

**Bug caught wiring the loop:** a backgrounded run has **closed stdin**, so the line
channel closes immediately — and `approve` was reading that `None` as a deny,
auto-aborting every call before an approver could send SIGUSR1. Fixed: a *closed* stdin
drops out of the `select!` and approval waits on the signals alone. This is the case
that makes the SIGUSR1 channel usable headless.

**Live round-trip confirmed (Haiku):** backgrounded run, argv-seeded turn → tool call →
waited at the gate → approved with `kill -USR1` → bash ran → result fed back → final
answer → clean exit (0) on stdin EOF.

### Phase 0 — COMPLETE (gate cleared) ✓

The §7 gate passed on a live multi-step run (`cat DESIGN.md` → `wc -l` → `git log`,
each approved via `kill -USR1`):

| req | input | cache_creation | cache_read |
| --- | --- | --- | --- |
| 1 (prompt only) | 665 | 0 | 0 |
| 2 (after `cat DESIGN.md`) | 6 | **9407** | 0 |
| 3 (after `wc`) | 6 | 165 | **9407** |
| 4 (after `git log`) | 6 | 89 | **9572** |

`cache_read` climbs (0 → 9407 → 9572) while `cache_creation` collapses after the one
initial write — the prefix is reused, only the small tail re-written each turn. Req 1's
665-token prefix was under Haiku's minimum and silently didn't cache (§3). **That
`cache_read` climbs *at all* is live proof projection is byte-stable.** Persistence
verified: the 8-entry session JSONL replays and `jq`s cleanly.

Phase 0 retired all four risks: loop works, projection byte-stable, cache reads
register, bash tool safe.

---

## 2026-06-23 — Phase 1+2: the universal redaction fork

**Design pivot.** An earlier "deferred keep-last-N + tail-rewrite + `Commit`" sketch
carried raw on the wire transiently and then time-travelled back to mutate it — which
forced all the `Commit` machinery. jbr flipped it to the cleaner shape: **redact every
result at append; the canonical wire never carries raw at all** (§4-realized). Because a
result's `wire` is fixed the moment it's appended and never changes, **no cached prefix
is ever mutated** → caching is trivially safe, exactly like Phase 0. `Commit` is not
needed here; it moves to Phase 3 compaction.

- **Slice 1 — stub distiller (BUILT).** `src/distill.rs`: a `Distiller` trait + a
  deterministic `HeadTail { head, tail }` stub (keep first *head* + last *tail* numbered
  lines), `cat -n` numbering with absolute numbers across the elision. Distill-at-append
  wired into `agent::run`. Proves the divergence + caching in isolation, no model call.

- **Phase 1+2 — the model fork (BUILT & verified live on Haiku and Opus).** 25 tests.
  - `Distiller` is now **async with fork context**: `distill(client, prefix, call, raw)
    -> WireState`. `ForkPeek` (the §4 model fork; default) projects the
    prefix-through-`tool_use`, places a cache breakpoint, appends the full
    line-numbered raw + a retain prompt, **forces one `retain` call**, parses `keep`
    (`all`/`none`/`"40-52,310-318"`) + `note`, and renders the distillate. On any
    failure → keep-all (never lose data).
  - `tool_choice` (`api.rs`): `{auto, disable_parallel_tool_use: true}` every request
    — **one tool call per turn** (§8); `force(name)` for the fork.
  - **System preamble** explains the curation, says *don't pre-filter*, "the distillate
    IS the output." `PEEK_LINE_CAP = 2000` is a safety floor on what the peek sees.
  - **Verified** (ferritin "find the request-header method signature" task): both models
    reached the exact answer (`Conn::with_request_header`, full signature). The fork
    redacts hard (Conn struct 134→13 lines; `--help` 79→8), and the `note` field behaves
    as a *running research log* carrying intent forward — §1 intent-aware compaction
    emerging on its own.

**Decisions locked.** "The distillate IS the output" — no "raw in log" / "re-read"
framing anywhere the agent or the peek can see it; it manufactured anxiety (Opus wasted
a turn trying to un-elide `--help`) and the old "can be re-read" line likely *caused*
over-dropping. The peek is now told *"whatever you drop is gone from the working context
— keep what might matter, stay lean."* **Findings:** the preamble lands on Opus (zero
pre-filtering) but not Haiku (reflexive `grep`/`head` — a weak-instruction-follower
artifact); pre-filtering never produced a wrong answer.

### Distillate inflation finding → diff-hunk rendering

Auditing a real Haiku session (24 tool results) surfaced a quantified problem: **for 13
of 24 results the distillate was *larger* than the raw it replaced.** Cause: the
canonical wire numbered every kept line `cat -n`-style (~7 bytes/line), and the common
verdict on small outputs is `keep=all` — so the distillate was raw + a number on every
line + the note, while *also* paying the extra peek round-trip.

**Fix (jbr's call):** drop per-line numbers from the canonical wire; carry position in
diff-style hunk headers (`@@ lines a-b @@`) instead. Per-line numbering stays in the
*fork peek view* (the coordinate system `retain(lines=…)` addresses), so the two
renderings split into `number_lines` (fork only) and `render_hunks` (canonical wire).
`retain({keep, note?})` schema unchanged. No cache/persistence impact (a distillate's
wire is computed once at append and stored). 30 tests. Also fixed in passing: the
`retain` tool *description* still carried the "can be re-read" framing the §8 decision
removed everywhere else.

---

## 2026-06-23 — Tool seam + fs-mcp / semantic-edit-mcp in-process

Driven by a live Haiku run that exposed three gaps (a no-tool-call when the prompt
didn't spoon-feed orientation; a wrong-language guess; a final answer truncated at
`max_tokens`). Built:

- **Orientation + env context.** The system preamble now orients the agent ("you are a
  coding agent in a Git repo at the cwd; explore it"), and `src/env.rs` appends a
  **session-fixed** block (working dir, platform, project type) computed once at startup
  and frozen into the system prompt — only session-stable facts, so it never invalidates
  the cached prefix (§3). Volatile state (git status, file lists) is excluded.
- **`max_tokens` 1024 → 4096** (`api.rs`) — 1024 truncated a real final answer.
- **The §5 Tool seam** (`src/tools.rs`). A `Provider` enum aggregated by a `Toolbox`
  (enum, static dispatch — no `async-trait` dep). The loop knows only `Toolbox::
  {definitions, run}`; bash is no longer special-cased.
- **fs-mcp + semantic-edit-mcp integrated in-process** (resolves the §5 Phase-4 open
  question). Approach: reuse the generated `mcplease` `Tools` enum; one additive
  `lib.rs` per crate exposes `Tools`, the state struct, and an `embedded(cwd)`
  constructor. `McpServer<State, ToolEnum>` (generic) holds state in a `RefCell`,
  dispatches via `from_value::<ToolEnum>` + `Tool::execute`, and translates `mcplease`'s
  `ToolSchema` into the Anthropic `input_schema` shape. semantic-edit integrated with
  **all** features (tree-sitter grammars not gated — accepted build cost). 40 tests.
- **Shared-store side effect, RESOLVED.** `FsTools::new` persists to a shared store
  (`~/.ai-tools/...`) other servers read; mounting would clobber it. Fix: `embedded(cwd)`
  builds the session store **in memory** (`SessionStore::new(None)` — no disk, no
  file-watcher). This also made the fs end-to-end test hermetic, so it un-`#[ignore]`d.
- **Curation forced:** `McpServer` filters `set_working_directory` (a `HIDDEN` list) —
  both servers expose it, so advertising would duplicate a name (the API rejects
  duplicates). A `mounted_toolbox_advertises_unique_tool_names` test guards collisions.

Still open: cargo-mcp not yet integrated (mcplease version skew 0.2.2 vs 0.2.3; shells
out to real `cargo`/`rustup`) — later moved *off* the critical path (§5).

---

## 2026-06-24 — Streaming the upstream wire (SSE)

The §8 streaming swap landed: the upstream call now sends `stream: true` and consumes
the response as SSE via `Conn::into_sse()`, replacing the single non-streaming POST. The
transport boundary stays put — `complete()` keeps its signature and returns one
`Completion`, so loop, projection, fork, and caching are untouched.

- **`api::Assembler`** — a pure fold over the event sequence (`message_start` /
  `content_block_*` / `_delta` / `_stop` / `message_delta` / `message_stop` / `ping` /
  `error`); `finish()` collapses to the `Payload::Assistant`. Reassembles `tool_use`
  input from arbitrarily split `input_json_delta` fragments. Unit-tested without a live
  stream.
- **Invariants preserved:** loud-on-unknown (unknown event/block/delta = hard error);
  usage merge (`input` + `cache_*` from `message_start`, `output_tokens` from
  `message_delta` — reading only one regresses the §7 gate metrics).
- **Thinking carved out, not built.** Streamed `thinking`/`redacted_thinking` and their
  deltas are recognized and rejected with a precise `THINKING_UNSUPPORTED` error — a
  clean landing spot. Storing thinking needs a `ContentBlock` variant, the mandatory
  same-turn round-trip (moves the byte-stability pin), and a resolution for the fork's
  forced `tool_choice` vs. adaptive thinking.
- **Error-body tradeoff (accepted).** `into_sse()` collapses a pre-stream non-2xx into
  `SseErrorKind::Status(code)` and discards the JSON body; operational overload still
  arrives as an in-stream `error` event whose full body we surface. Filed as a
  trillium-client backlog item.
- **Verified live (Haiku):** streamed assistant turn → `tool_use` input reassembled from
  fragments → approved via SIGUSR1 → bash ran → fork-distilled → second streamed turn →
  final answer; session JSONL replays clean. 46 tests. (Cache columns were 0 — the toy
  session's ~3.2k prefix is under Haiku's 4096 floor, not a regression.)

---

## 2026-06-24 — Control channel: HTTP over a Unix socket

Closes the §8 interface-parity gap. The `Approval::Redirect` steer was reachable only
through an interactive terminal, violating "no TUI-only capability." The settled shape is
**stronger than parity**: the HTTP interface is the *singular, authoritative input
source* — no privileged in-process path; the loop's only `Frontend` is the remote one.

**The thesis payoff: the log is already the event store.** SSE has no replay, so
reconnect drops events — but the append-only log with monotonic `seq` *is* the event
store. `GET /events` tails the log; reconnect = `GET /log?since=<seq>` then tail. The
durable design and verb set are now in `DESIGN.md §10.1`.

### Slice 1 — `Shared` + `RemoteFrontend` + write verbs (BUILT & verified)

`src/daemon.rs`: `Shared` (the loop ↔ handlers seam, `Arc<Shared>` in trillium's
server-global `shared_state`), `RemoteFrontend`, a hand-routed `handler`, `serve()`.
`agent::run` now takes `Arc<Shared>` and reads the **first** turn over the socket too
(argv-seeding gone); the log moved behind `Arc<Mutex<Log>>` (never held across an await
— the fork gets a `snapshot()` clone). `Interactive` / stdin / SIGUSR1 / `.efference.pid`
**retired**. `main.rs` is clap now: `efference serve [--socket]`. Verbs: `POST /turn` →
202; `POST /approvals/{call_id}`; `POST /interrupt`; `GET /state`; `GET /log?since`.
**Verified live (Haiku) over `curl --unix-socket`** end to end. 41 tests, clippy clean.

### Slice 2a — client + scriptable verbs (BUILT & verified)

The blocker — trillium-tokio shipped no *client-side* Unix connector — was cleared by
jbr writing `trillium_tokio::UnixClientConfig` (a fixed-path-dialing `Connector`,
released in **0.6.4**). `src/client.rs`: one method per verb, each surfacing the daemon's
own JSON error body verbatim. **Daemon re-routed onto `trillium-router`** (real
`:call_id` capture); the hermetic client test dogfoods the same router. CLI verbs
`efference turn` / `approve [--abort | --redirect]` / `interrupt` / `state` / `log`; a
bare `efference approve` defaults to the one pending gate — **what replaces the SIGUSR1
hack**. **Multi-turn headless confirmed** (a second `efference turn` answered from
context, no tool call) — retiring the "backgrounded run can't get its opening prompt"
constraint that demanded this channel. 43 tests.

### Slice 2b — `GET /events` SSE + `attach` REPL (BUILT & verified)

Clients now tail the log live instead of polling. `GET /events` is an SSE tail of the
append-only log (`?since=<seq>` replays then follows live; `event: <kind>`, `id: <seq>`,
`data: <serialized LogEntry>`). **We tail the durable log rather than a broadcast
channel**, so a slow client never drops events. The stream is a `futures_lite` `unfold`
that is **lost-wakeup-safe** (the `Notified` is created before the catch-up read).
`efference attach` is the first interactive client: a background task tails `/events` and
renders; the foreground reads stdin and per line resolves the gate or sends a turn.

**The trillium-http fix that unblocked this (root-caused live).** `/events` first
returned **zero bytes** while `/log`/`/state` worked. Root cause was *not* in efference
or trillium-sse: **`trillium-http`'s body→socket copy loop only flushed on EOF**, so an
endless SSE stream's events sat in the `BufWriter` forever. Fix (patched in via
`[patch.crates-io]` to a local checkout): flush the writer when the body yields
`Poll::Pending`. **(2026-06-25: the upstream flush fix shipped — the `[patch.crates-io]`
is now removable; drop it next time `Cargo.toml` is touched.)** **Verified live (Haiku):**
`curl -N /events` streamed events the instant a turn was posted; the approval gate over
`attach` works end to end. 44 tests.

---

## 2026-06-24 — Cache-aware transport: instrument panel + keepalive

Caching is the §3 constraint the whole design bends around, yet it was invisible (usage
was `eprintln!`'d and dropped) and the cache died at the 5m TTL on any idle. This slice
makes it **legible** and **keeps it warm**, entirely in transport/daemon — never touching
the log or a projection, so it can't perturb the byte-stability pins (§3).

**Two live probes settled the mechanics first** (~3¢, before any policy code):
- **`max_tokens: 0` reads the cache** — returns `cache_read > 0`, `output: 0`, instant: a
  genuine cache-hitting use that refreshes the sliding TTL while generating nothing. **The
  keepalive primitive.**
- **Cache TTL is a WRITE-time property — no cheap read-upgrade.** A `ttl: "1h"` request
  against a prefix cached at 5m returned `cache_creation: 0`, `cache_read: 14402`,
  `ephemeral_1h: 0` — it just *read* the 5m entry. A warm 5m cache **cannot** be lifted to
  1h. **Decision: the 5m keepalive is the tool; 1h is dropped** as a keepalive concern
  (becomes a future session-init knob). This killed the "stepping-away → bump to 1h" idea
  before it was built.

**Instrument panel.** `Shared` holds a transport-only `metrics` cell (latest `usage`,
cumulative totals, two wall-clock `Instant`s — never serialized, only derived elapsed
seconds). `GET /state` now carries a `usage` block. The fork peek's usage is folded in (it
reads the shared prefix and so slides its TTL). **Keepalive.** `keepalive_loop` (spawned
in `serve`) fires a non-streaming `max_tokens: 0` round-trip over the byte-identical
prefix when the cache nears eviction at idle. **Two clocks:** a *cache-touch* clock
(turns + fork + pings) and a *real-turn* clock (turns + fork only) — a ping refreshes the
TTL but doesn't reset its own give-up deadline. Pings at `PING_AT` (240s), stops past
`CAP` (30 min idle, the §3 crossover where one cold read on resume wins).

**Verified live (Haiku).** Cold write of a 6149-token entry → idle → at ~235s no ping →
keepalive fired (`cache read 6149 — TTL refreshed`) → `/state` showed the ping's usage
and `seconds_since_last_request` reset 234 → 65. The timer fires only at genuine idle.
46 tests.

---

## 2026-06-25 — Token-level streaming (deltas)

The "token-level deltas, cheap to add later" note is built — realized as the insight that
**assistant prose and tool stdout are the same thing**: ephemeral, pre-commit output the
committed log entry supersedes on commit. One mechanism carries both.

- **The side channel.** `Shared` gained a lossy `broadcast::Sender<StreamDelta>`
  (`AssistantText` | `ToolOutput { call_id, chunk }`). A new `GET /stream` SSE route
  subscribes per connection. Deliberately **lossy** where `/events` is durable (the log is
  the backstop); delta events are **id-less** (the contract keeping them off the `/events`
  resume cursor); `StreamDelta` is never appended/projected — transport-only.
- **Producers stay daemon-ignorant.** `api.rs::complete` and `bash.rs::run` each take a
  plain `impl FnMut(&str)` sink; only the loop adapts them into `StreamDelta`. **The fork
  peek passes a no-op sink** — its triage tokens never surface.
- **Bash streams both pipes.** `wait_with_output` gave way to concurrent incremental reads
  of stdout+stderr via `tokio::select!` — output is now **arrival-interleaved** (more
  terminal-faithful; streams stderr progress like cargo). The process-group abort path is
  preserved.
- **Client/TUI.** `attach` renders `/events` and `/stream` from a **single merged task**
  (`render_streams`), which lets the spartan client dedup without a full TUI: live
  fragments print first, the committed entry finalizes the line. Tool output is *not*
  deduped against the committed `tool_result` (the stream shows raw live output, the
  result shows the fork distillate — both carry signal).
- **Tests:** `bash::streams_chunks_to_the_sink` and `daemon::stream_carries_ephemeral_
  deltas`. **55 green, clippy clean.** Upstream streamed-prose path verified live through
  `attach` (Haiku) with the original two-task renderer; the merged-renderer dedup that
  replaced it is tests-green but wants one more live `attach` pass.

---

## 2026-06-25 — RE-READ resolved (design note)

The re-read affordance was deferred because the *obvious* implementation — flipping a
committed result's `WireState` back to a fuller distillate/`Raw` — mutates a cached prefix
entry and invalidates everything downstream (the cardinal sin, §3). Resolution: a
`reread(seq, focus?)` tool re-runs the **existing `ForkPeek`** on that entry's raw and
**appends the new distillate at the tail** — pure append, cache-clean. It reuses the
universal fork verbatim; the same call serves "more of X" and "all of it now"
(`retain(all)`). Addressed by the visible `seq` handle (block addressing). Build trigger
stays gated on evidence (over-redaction causing a wrong answer); the cost is now low enough
to ride alongside Phase-3 Slice 2. Folded into `DESIGN.md §4`.

---

## 2026-06-25 — Phase 3 Slice 0: the `consolidate` tool (commit `4bf649b`)

`src/consolidate.rs` + loop wiring (`DESIGN.md §10.4`). `consolidate` is an **agent-called
tool** — advertised in `tools[]` but dispatched **structurally** in `agent.rs`, not via the
world-facing `Toolbox`: it is the first tool that acts on the **log**, not the world. The
agent calls `consolidate(summary)` to fold its finished work; on approval the loop appends a
`Mark { Boundary { upto }, note: summary }` + a paired `tool_result` ("consolidated").
`upto` (`consolidate::extent`) is the entry just before the most recent `User` turn, so the
live task instruction always survives. `Mark` / `MarkKind::Boundary { upto }` added to
`src/log.rs`. `branch-from-seq` rides the same substrate. The tool description is framed per
"distillate IS the output" — the summary *becomes* the record, no "you can re-read it."

## 2026-06-25 — The `[seq]` handle + first dogfooding pass (commit `ccc51f5`)

Tool results now carry a visible `[<seq>]` prefix on the wire (`[2] file.txt`) — a
render-time handle making "reread 14" / "branch from 14" describable without a schema
change; the expected-wire-bytes pin was updated to include it (`DESIGN.md §4`). **First
dogfooding session captured** in `feedback/session-observations.md` (efference exploring its
own codebase over the daemon). What landed well: focused output, hunk rendering, the `[seq]`
prefix, search-with-context. Rough edges flagged: no progress signal during a fork peek on
big files; the `note` field never surfacing to the human; uninformative elision markers
(`… [N lines elided] …` doesn't say *what* was dropped); and the clunky `kill -USR1` UX
(since superseded by the daemon). These seed the backlog (distillation breadcrumb, peek
progress indication, smarter elision markers).

## 2026-06-25 — Model is a session flag (commit `985549c`)

`efference serve --model <id>` (default `claude-haiku-4-5`); the model was a hardcoded const
before. Opus 4.8 remains the locked *real* default — the flag is a dev convenience. Fixed
for a session (changing it invalidates the whole cache, §3).

## 2026-06-25 — Phase 3 Slice 1: the projection fold, multi-boundary (commit `e77aa41`)

The big `project.rs` rewrite. A pre-pass (`extract_boundaries`) collects every
`Mark::Boundary`; `project` then elides every `seq <= upto` (plus the mark itself) and
injects the `summary` as a user-role message **just before the first live entry past
`upto`** (the head of the live conversation, not the mark's own log position). **Multiple
consolidations overlap** — each later fold subsumes the earlier ones, and each emits its own
summary, so the wire reads summary-A · summary-B · … · live turns. The `consolidate`
`tool_use` / result pair projects normally ("keep it visible" resolved). The tail edge (a
consolidate with nothing after it yet) still injects the summary. Tests cover the single
fold, multiple overlapping folds, the tail edge, and the mechanical span fold. `project`
keeps its no-`upto` signature — the fold is driven entirely by the marks in the log.

## 2026-06-25 — Token-level streaming committed (commit `706307b`)

The token-delta work landed: `GET /stream`, the lossy `broadcast::Sender<StreamDelta>`, bash
both-pipes arrival-interleaving, and the merged-renderer `attach` (`render_streams` +
`RenderState`) that dedups live prose against the committed entry. The upstream
streamed-prose path was verified live earlier with the two-task renderer; the merged
renderer is tests-green and wants one more live `attach` pass to confirm the echo-suppression
reads cleanly.

---

## 2026-06-25 — Interactive TUI (M1): vendor ferritin's renderer

Went from no TUI (the spartan `attach` REPL — an SSE tail piped through `println!`) to a
themed, scrolling, markdown-rendering interactive client, by vendoring-and-adapting
ferritin's `renderer::interactive` into `src/tui/` (~26 modules). The load-bearing seam
ferritin exposed carried over near-verbatim: a general-purpose document IR
(`tui/document.rs`, ex-`styled_string`) rendered by a **domain-free** pipeline
(`render_node` / `render_span` / `render_code_block` / `render_table` / `render_scrollbar` /
`write_text`) over a syntect theme (`tui/theme.rs` + `render_context.rs` + `color_scheme.rs`).
A grep of the whole `render_*` family for ferritin domain types returned one hit (`theme.rs`
→ `RenderContext`) — that's what made this a lift, not a rewrite.

- **New `efference tui` subcommand** (`src/main.rs`) — a read-only viewer. A tokio task
  bridges the daemon's `/events` (durable `LogEntry`) + `/stream` (`assistant_delta` /
  `tool_output`) SSE into a crossbeam channel; the crossterm UI loop runs on a dedicated
  blocking thread (`tui::run` → `ui_loop`). The daemon *is* the request side, so ferritin's
  two-thread request architecture (Navigator / Request / Commands / `request_thread`) is
  dropped entirely.
- **Projection** (`src/tui/projection.rs`) — `project(&[LogEntry], &LiveTail) -> Document`:
  the transcript as another `project(log)` (§10, sibling of the wire projection). Mirrors
  `project.rs`'s distillate-else-raw rule; wraps tool output + call input in collapsible
  `TruncatedBlock`s. A `LiveTail` holds in-flight streaming deltas, superseded by the
  committed entry (the structural form of `attach`'s `streamed_*` dedup). Re-projects per
  inbound event; auto-scrolls while following the stream (`stuck_to_bottom`).
- **Theming** — 22 themes baked at build time (`build.rs` + 14 git submodules under
  `assets/themes/`, registered to match ferritin); live theme picker (`t`); startup theme
  via `--theme` / `EFFERENCE_THEME`, defaulting to "Catppuccin Frappe" (not the sort-first
  "1337"). An unknown name errors before the alt-screen with the available list.
- **Input** (M1 keyset) — `j`/`k`/PgUp/Dn/mouse scroll + the custom scrollbar; `Tab`/`Enter`
  expand collapsible blocks; `t` themes; `?` help; `q` quit.

**Verified.** 79 lib tests green (vendored markdown/table tests + new projection tests: all
payload types, distillate-supersedes-raw, live-tail commit, and a `TestBackend` render
asserting transcript text reaches the screen). Live e2e against Haiku: drove a streaming
turn — haiku prose streamed token-by-token, paused at the `bash` gate, tool output (`26`)
streamed after approval, closing sentence streamed — confirmed both in an automated pty
capture and by the user watching live. Path: daemon → SSE → bridge → re-project-per-token →
ratatui render.

**Decisions.** (1) **Vendor-and-adapt, share later** — copy into `src/tui/` and edit into
shape; extract a shared crate only if a clean core emerges. The rustdoc appendage
(`TuiAction::Navigate` / `Metadata` / `DocRef`, the markdown link resolver) was stripped; the
IR is now markdown + code + collapsible blocks. (2) **Kept the IR's `Cow<'a>` lifetimes**
rather than converting to owned `String`: streaming re-projects the whole `Document` (it
never mutates nodes), so owned-mutation bought nothing, and keeping the pipeline
byte-identical to ferritin maximizes backportability. Revisit if the M2 editor wants in-place
mutation. (3) Themes as **git submodules**, not vendored files (faithful to upstream,
preserves LICENSEs). Deps added: `ratatui` 0.30, `crossterm` 0.29, `crossbeam-channel`,
`syntect` 5.3, `pulldown-cmark` 0.13, `percent-encoding`, `unicode-width`, `fieldwork`,
`thiserror`.

**Known M1 limits (by design).** Read-only — input is M2 (a tui-textarea composer behind a
swappable `Composer` seam), approvals M3. A block expanded mid-stream resets on the next
token (expand state lives in the doc, which re-projects) — fine when idle; moving it into
`state` is the fix. `OpenUrl` is a no-op (no `webbrowser` dep).

---

## 2026-06-25 — TUI M2 + M3: composer & in-TUI approvals

Same session as M1; the TUI went from read-only viewer to a full interactive client.

- **M2 — input composer** (`src/tui/composer.rs`). `tui-textarea` was the planned first cut
  but is pinned to ratatui 0.29 / unicode-width 0.1 and won't resolve against our ratatui
  0.30 — so the composer is a small **dependency-free** emacs-keybind editor behind a
  swappable seam (`handle_key` / `render` / `text`): a `Vec<Vec<char>>` buffer with
  C-a/e/f/b/p/n/k/u/w/d, Alt-f/b, arrows, Home/End, Backspace; Enter submits, C-j/Alt-Enter
  inserts a newline. **Focus model** (the user's choice): an Esc-toggled *compose* vs
  *command* mode — compose routes keys to the editor (PgUp/PgDn still scroll); command
  restores M1's single-key transcript commands (`j/k/t/?/q/Tab`), and any printable key (or
  `i`) returns to compose. `render_frame` carves a growing input area above the status bar
  (the `mouse.rs` height math moved in lockstep); submit drives `POST /turn` over a tokio
  `mpsc` → the daemon.
- **M3 — in-TUI approval gates.** The gate is ephemeral control state with no SSE event
  (DESIGN §10.0 parks "pending-approval as synthetic `/events`"), so a poll task watches
  `GET /state`'s `pending_approval` and emits a `Gate` event only on change. A highlighted
  banner renders above the composer; submitting while gated resolves it with the same
  three-valued [`Approval`](src/frontend.rs) the loop consumes — empty = approve, `.abort` =
  abort, other text = redirect (the `attach` REPL's semantics, now in the composer). An
  empty Enter still approves (the composer would otherwise swallow it). Decisions flow over
  a second tokio `mpsc` → `POST /approvals/:call_id`.

**Verified.** 92 lib tests green (+13 over M1): composer editing/motion/kill/submit (7),
gate `/state` parse + null (2), and the gate-aware submit mapping incl. approve/abort/
redirect/normal-turn (4). Live against Haiku: composer turn observed landing in the TUI by
the user and confirmed server-side (`#8 User "…composerworks" → #9 Assistant "composerworks"`);
a tool-calling turn produced a pending gate visible to the poller. (Interactive keystroke
capture under a headless pty proved flaky — the real-terminal check is authoritative.)

**Decisions.** Hand-rolled composer over `tui-textarea` (forced by the ratatui-0.30 version
gap, and aligned with the user's plan to own/rewrite the editor anyway — spellcheck,
typeahead, integrated focus). Gates via `/state` polling for now; synthetic `/events` is the
cleaner future. No new deps.

---

## 2026-06-25 — TUI M4: the conversational rendering pass

A display-iteration session driven by the snapshot harness — the screen visible at every
step instead of narrated by hand. Reframed the transcript projection from a flat dump of
labeled blocks toward a deliberate **conversational, minimalist** information architecture
(DESIGN §10.5).

**The iteration loop (built first, used throughout).** `tui/harness.rs`'s `Screenplay`
gained transcript-authoring helpers (`user` / `assistant` / `tool_result` / `tool_call` /
`expand_all`) and — because the framed character grid drops color by design — `cell(x,y)` /
`find(needle)` to assert the fg/bg/modifiers a snapshot can't show. Caught a latent gap: the
harness built a `RenderContext` with an *empty* `ColorScheme` (everything gray), so it now
loads the real `DEFAULT_THEME` — color assertions are faithful to the app. `efference
gallery` (`tui/gallery.rs` + `run_gallery`) launches the real TUI on a fixed "one of each
element" transcript with no daemon/API key — a live design page.

**Rendering fixes the gallery surfaced.** Markdown strikethrough rendered literally
(`Options` lacked `ENABLE_STRIKETHROUGH`); ordered lists rendered as bullets (`Tag::List`'s
start ordinal was discarded — added `start: Option<u64>` to the `List` node, numbered in the
renderer); the error header was `Strikethrough` (reads as deletion) → a dedicated
`SpanStyle::Error` (red). A `color_for_scope_with_fallback` bug leaked the theme's plain fg
when a scope was undefined → added `color_for_scope_or` (compares against the theme default).

**Per-tool representations.** Tool calls/results are known at compile time, so each gets a
shape over a JSON/text dump: `bash` → shell block; `read` → sentinel framing stripped +
highlighted by file extension; edit tools' `===DIFF===` → a colored `DocumentNode::Diff`
(green/red, verified via channel-dominance with the `cell` tool). Threaded `call_id → tool`
identity through `project()` to pair a result with the tool that produced it.

**The conversational IA (the headline).** After a design exploration (mocked three layouts
as `AskUserQuestion` previews; the user picked the chat-like one), introduced a `Turn {
author, blocks }` IR. Authorship is **structural** — a blue accent gutter for the user's
turn, the assistant unmarked — replacing `▌ you` / `▌ assistant`. Each call is **paired with
its result** into one collapsible *exchange* headed by `❯ <tool> · <arg>` (no `result #N` /
seq label); the leading glyph unifies state: `❯` at rest, red `✗` on error, `▶`/`▼` when
collapsible. Collapse is now a **toggle** (was a `Full → Full` ratchet) — peek then re-close.

**Verified.** 118 lib tests green (+26), clippy clean (the 2 remaining warnings are
pre-existing in `mouse.rs`). New cell-style assertions lock what snapshots can't: user-gutter
blue, error-prompt red, diff green/red, strikethrough crossed-out. The `gallery` snapshot is
the regression guard for the whole element set.

**Decisions / deferred.** Staged: the conversational frame landed; swapping the result's
syntax-highlighted *box* for a lighter `│` connector gutter (the chosen mockup's look) is
the pending Stage-2 refinement. Dropped the unused `screen_width` param from
`estimate_node_lines` rather than rename it. No new deps.

---

## 2026-06-26 — Ambient notifications, Slice A: the verdict refactor

Design converged with jbr on the **ambient/redactable notification system** (inbox
semantics; event-sourced, logged at first-show; pin/dismiss/leave with leave-as-default;
`slot` archive-and-replace supersession; pin-at-`Triage` placement; the fork reframed as a
*perception manager* — manages what the agent perceives next, never acts on the world).
Folded into `DESIGN.md` §2/§3/§4/§6 + the §10.0 slice plan (A verdict-refactor, B
ingestion, C inbox-in-peek). Then built **Slice A** — the load-bearing type change, against
the existing tool-result fork, no notifications in the loop yet.

- **`WireState::Distilled` carries the verdict, not bytes** (`{ keep: Keep, note }`, was a
  rendered `String`). `project::render_distilled` re-renders the `@@`-hunks from `raw` + the
  verdict each turn, so the kept bytes are never persisted twice and the wire is genuinely
  `project(log)` recomputed (§4). `render_hunks`/`normalize` moved from `distill.rs` into
  `project.rs`; the distiller now *produces a verdict*, `project` *renders* it.
- **Fork verdicts are now logged** as `Payload::Triage { for_result, origin, raw_keep,
  inbox }` (§4): the throwaway peek's *transcript* stays throwaway, its *decision* is
  durable — so every fork decision is reconstructable for audit/introspection. `origin`
  (`Model` vs keep-all `Fallback`) recovers the one signal the rendered artifact erased
  (a deliberate keep-all and a failed peek render identically). `inbox` empty until Slice C.
- `Distiller::distill` returns a `Distillation` struct; `Log`/`Shared::append` now return
  the assigned `Seq` so the loop can key the `Triage` to its result. Fixed the `attach`
  client's loose-JSON reader, which assumed `Distilled` was a string (would have shown raw
  instead of the distillate).

**Verified.** Clean build; **125 lib tests green** (byte-stability pins unchanged — the
verdict change doesn't perturb the wire; added `render_distilled` truncation + the moved
render tests). 2 TUI snapshots regenerated (distilled fixtures → the `— <note>`
breadcrumb). clippy auto-fixed 2 pre-existing collapsible-`if`s; 1 pre-existing
large-`Err` warning left (`tui/mod.rs:340`, out of scope). **Live-verified against Haiku:**
a `printf`-5-lines turn → the fork kept line 3 (`wire: Distilled{keep:Ranges[[3,3]],
note:"…cherry"}`), logged `Triage{for_result:2, origin:Model, raw_keep:"3", inbox:[]}`, and
the wire rendered `[2] @@ line 3 @@ / cherry / — …` with the other fruits off the wire — the
agent answered from the distillate. The §4 disk/thesis win + fork-verdict audit, confirmed
end to end.

### Slice B — the inbox (ingestion)

The ephemeral notification queue + its control verbs, no model in the loop yet. `Shared`
gained an `Inbox` (a queue-local-id counter + a `Vec<PendingNotification>`); `POST /notify
{source, body, slot?, replaces?}` enqueues with **slot supersession** — a later notify on the
same `slot` archives-and-replaces the pending one (the "running 5min" → "finished" case,
never an edit, §3); `POST /notifications/:id/dismiss` clears one out-of-band; `GET /state`
grew a `pending_notifications` array. Client methods (`notify`/`dismiss_notification`) + CLI
verbs (`efference notify <source> <body> [--slot] [--replaces]`, `efference dismiss <id>`)
round it out — the §8 producer path (a background process / operator / another agent posting
to the socket). Pending entries are pure control state (the pending-approval precedent,
§10.1): a superseded/dismissed-while-pending notification is dropped, never logged — Slice C
logs only what a gate actually *shows*.

**Verified.** +2 tests (**127 green**), clippy clean of the new code: an inbox unit test
(slot supersede + dismiss-by-id) and a full socket round-trip (`POST /notify` ×2 with slot
supersession → `GET /state` shows one → dismiss → empty; dismissing the superseded id 404s)
over the real router + control client. No live API run — Slice B has no model interaction;
the live pass lands with Slice C, where notifications flow through the fork peek.

### Slice C — the inbox in the peek

Notifications reach the model. At each fork gate the loop opens a triage step: a new
`inbox.rs` folds the log to the **active** inbox (`active_shown` — `Ambient` arrivals minus
the seqs any `Triage` has pinned/dismissed), re-showing "left" notifications; newly-arrived
ones are drained from the ephemeral queue and appended as first-show `Ambient` entries (each
seq becoming its durable id). The peek view lists the inbox by `[seq]` under the
line-numbered output; the forced `retain` tool grows an `inbox { pin, dismiss }` field (only
when there's an inbox to triage); the dispositions ride back on the `Distillation` and land
on `Triage.inbox`. `project` emits a `⟢ source: body` user block per **pinned** notification
at its `Triage`'s position (body looked up from the `Ambient` arrival), tail-ward so a
pin-at-a-later-gate never splices behind a breakpoint (§3); dismissed/left ones never project
(the human TUI renders every shown notification, the wire only the pinned). "Leave" is the
absence of a disposition — the notification stays active and re-shows next gate.

**Verified.** +5 tests (**132 green**), clippy clean: the `active_shown` fold (resolves
pin+dismiss, keeps left), the pin-at-`Triage` projection (pinned `⟢` block at the verdict
position, dismissed off-wire, arrivals project to nothing), and `retain` inbox parsing.
**Live-verified (Haiku):** with a tool call pending, posted three notifications — two on one
`slot` (the second superseded the first in the ephemeral queue, live) + a noise one. On
approval the gate logged the two survivors as `Ambient` (seq 2–3), showed them in the peek,
and the model **pinned** the CI build failure and **dismissed** the chatter
(`Triage.inbox=[[2,Pin],[3,Dismiss]]`). The pin rendered `⟢ ci: build #413 FAILED…` on the
wire (the dismissed one absent), the inbox drained to empty, and the model's answer surfaced
the build failure — it acted on the pinned fact. The full §6 inbox, end to end.

### Background processes — the supervisor, Slice 1 (§6)

The §6 supervisor lands on the inbox seam, with the **hybrid record model** decided in design:
specialize the ephemeral (a live `Supervisor` registry), reuse the durable (`ToolResult` for
the spawn, `Ambient` for exits and pinned tails) — **no new `Payload` variant**. A
backgrounded command is a *live entity*, not a log record: its running tail is re-sampled
fresh each triage gate, never frozen into the log (forcing a live tail through the immutable
notification model would pile up stale snapshots, §6).

- **Substrate** (`src/supervisor.rs`, new) — `Supervisor` registry (sibling to `Inbox` /
  `Metrics` in `Shared`): one bounded per-process capture buffer (drop-oldest-with-breadcrumb,
  no silent caps), `spawned_at` / `last_chunk_at` timing, mute + running state. Read
  projections: `views()` (the ambient auto-show — running, un-muted, fresh tail + `running`/
  `idle` secs), `pin_snapshot()` (a frozen tail for a pin), `finish()` (the exit notification
  body — status + duration + final tail). 6 unit tests.
- **Drainer reuse** (`src/bash.rs`) — the foreground `tokio::select!` pipe loop is extracted
  to `drain_pipes(stdout, stderr, cancel, on_chunk) -> cancelled`, shared by the foreground
  bash tool (accumulate + race abort) and the background drain task (feed the buffer, never
  cancel — a kill closes the pipes externally). `kill_group` is now `pub(crate)` for teardown.
- **Orchestration** (`src/daemon.rs`) — `Shared::spawn_background` spawns into a fresh process
  group and detaches a drain task that feeds the registry and, on exit, fires the `Ambient`
  exit notification via the existing `notify` path (slot `bg#<n>`). `bg_snapshot` / `bg_pin_snapshot`
  / `mute_bg` / `kill_all_bg` (teardown reaps live children, §6 ownership).
- **Spawn + triage** (`src/agent.rs`, `src/bash.rs` def) — `bash` gains a `background: true`
  flag; the loop routes such a call to the supervisor (returning a `bg#<n>` handle) and still
  distills the tiny spawn result through the universal fork (no bypass). The live tails ride
  the same peek.
- **Peek + verdict** (`src/distill.rs`) — the fork view grows a `═══ background · n running ═══`
  section (each process by `[bg#n]` with tail + timing), re-rendered every gate off the cached
  prefix so it never touches the canonical wire (§3). `retain` gains a handle-keyed `background
  { pin, mute }` field (distinct from the seq-keyed `inbox`). A `pin` snapshots the current tail
  into an `Ambient` and folds a `Pin` into the inbox verdict — reusing the notification
  pin-at-`Triage`-position projection (**zero `project.rs` changes**); a `mute` flags the
  supervisor (process keeps running, exit still notifies).

**Verified.** +7 tests (**139 green**), no new clippy warnings. The supervisor unit tests
(register/tail/mute/finish/pin/front-drop) plus a daemon integration test
(`background_process_drains_and_notifies_on_exit`) that runs the real spawn→drain→exit→notify
path with no model: a backgrounded command spawns, drains, and on exit lands an `Ambient` in
the inbox carrying `exited 0` + the final tail on slot `bg#1`.

**Live-verified (Haiku).** One turn: "background a 20s ticking loop, then `ls -la`." The model
called `bash{background:true, command:"for i in $(seq 1 20)…"}` → spawned `bg#1`, returning
immediately. At the **spawn gate** the live tail already surfaced (`[bg#1] … running 1s` with
`tick 1`/`tick 2`) and the model **pinned** it — which projected `⟢ bg#1: … · running 1s\ntick
1…` on the **canonical wire** at the `Triage` position (the pinned tail, the `⟢` sigil, reusing
the notification pin path). The loop kept running; by the **`ls` gate** ~20s later it had exited,
and the **exit notification** (`exited 0 after 20s; final output: … [8 earlier lines]\ntick 9…`
on slot `bg#1`, the front-drop breadcrumb intact — no silent cap) had drained into the inbox,
where the model **dismissed** it (clean exit, nothing to do) — staying audit-only, off the wire.
SIGINT shutdown ran `kill_all_bg` cleanly (exit 0). All four requirements, end to end. (The
`·`/`…` are correct UTF-8 in the log — U+00B7 / U+2026; a terminal-only mojibake during
inspection, not stored.)

### Background processes — the TUI process panel, TUI-bg-1 (§6, §10.5)

The human's view of the supervisor — so the TUI is no longer blind to a subsystem that, on
`/events`, shows only the lifecycle bookends (the spawn `ToolResult`, the exit `Ambient`); the
live running state in between is deliberately unlogged. **No new endpoints:** the running tail
is ephemeral, so it rides `/state`, not `/events`.

- **Transport** (`src/daemon.rs`, `src/supervisor.rs`) — `BgView` derives `Serialize`; `GET
  /state` grows a `processes` array (the same `views()` snapshot the peek auto-shows the model —
  one view, two consumers).
- **TUI** (`src/tui/{mod,state,render_frame}.rs`) — the existing 400ms `/state` poller (built
  for approval gates) now also parses `processes` and emits a `TuiEvent::Processes` on change
  (running/idle secs tick, so a live process refreshes ~1Hz — the cadence the panel wants).
  `InteractiveState` holds the list as **chrome, not transcript** (updates redraw, never
  reproject — the tail is never in the log). `render_frame` reserves a capped row band between
  the transcript and the gate/composer and renders one row per process: `▶ bg#n  command ·
  running · idle │ <last output line>` (handle/header in the accent style, last line muted),
  hidden when none run.
- **Decided:** reuse the single lossy `/stream` (a future `BgOutput { handle, chunk }` variant,
  demuxed by handle like `ToolOutput { call_id }`) for smooth live tailing (TUI-bg-2) rather
  than one SSE channel per process — selective subscription buys nothing for a local
  all-processes TUI over a UDS.

**Verified (test).** +2 tests (**141 green**), no new clippy warnings. A `Screenplay` snapshot
(`background_process_panel`) pins the rendered strip (two processes with timing + last line,
above the composer), and a daemon test (`state_json_surfaces_running_processes`) confirms a real
backgrounded child appears in `/state` with its handle/command/timing — the data path the poller
reads. (The supervisor end-to-end was already live-verified; this is the human projection of the
same registry.)

### Background processes — directed reads + teardown, Slice 2 (§6)

The model's *directed* access to the supervisor + inbox, complementing the ambient auto-show:
four harness tools dispatched structurally in the loop (like `consolidate`), but whose results
distill through the universal fork like any tool (no bypass).

- **Tools** (`src/supervisor.rs` defs, `src/agent.rs` dispatch) — `list_processes` (running
  processes with timing, muted ones annotated), `read_process` (the §6 `read_since`:
  cursor-based tailing of one process's bounded capture — returns the delta from a byte cursor
  plus a new cursor, reporting any front-dropped bytes, never silently re-paying), `kill_process`
  (`killpg` the group; the drain task then fires the exit notification), and `check_inbox` (the
  directed inbox read — surfaces pending notifications on demand so the model needn't manufacture
  a tool call to trip a gate). Names dodge the fs-mcp `read`/`list` collisions.
- **`Shared`/`Supervisor` methods** (`src/daemon.rs`, `src/supervisor.rs`) — `list_text`,
  `read_since`, `pgid_of` → `kill_bg`, and `inbox_summary` (active shown ∪ pending ephemeral).
- **Cache-correctness fix** (`src/agent.rs`, `src/daemon.rs`, `src/main.rs`) — the `tools[]`
  array is now built from one source, `agent::tool_definitions(&toolbox)` (world tools +
  `consolidate` + the §6 tools), shared by the agent loop, the keepalive ping, and the `wire`
  dev view. Previously the keepalive sent `toolbox.definitions()` *without* `consolidate`, so its
  `max_tokens:0` ping warmed a different prefix than real turns read — silently defeating idle
  cache-warming on the tools tier (DESIGN §3). Now byte-identical across all three.

**Verified (test).** +5 tests (**146 green**), no new clippy warnings. Supervisor units
(`list_text` running/muted, `read_since` cursor-advance + no-new + delta, `pgid_of` only-running)
and a daemon test (`kill_bg_and_inbox_summary`: rejects unknown handles, kills a running child,
and the inbox read surfaces a posted notification).

**Live-verified (Haiku).** Two turns. Turn 1 backgrounded a 16s `for`-loop, then `list_processes`
/ `read_process` / `kill_process` — the loop finished mid-gating, so this exercised the *exit/
empty* paths: `list_processes` → "No background processes running", `read_process bg#1` → the
captured buffer with a `bytes 0–311 (exited)` header + cursor, `kill_process bg#1` → "no running
background process" (and the exit notification `exited 0 after 16s` had drained into the inbox,
which the model pinned). Turn 2 backgrounded an *infinite* `while true; …` loop to hit the *live*
paths: `list_processes` → "1 background process(es) running: [bg#2] … · running 21s · idle 0s",
`kill_process bg#2` → "killed bg#2 (SIGKILL to its process group)", after which `/state` showed no
processes and the exit notification `killed by signal after 37s` was queued (pending the next gate
— no idle-triage until Slice 4). `/state` also showed `bg#2` live mid-turn — the TUI panel's data
path, confirmed live. Clean SIGINT teardown (exit 0).

### Background processes — idle-triggered triage, Slice 3 (§6); blocking `wait` dropped

**Design decision:** the planned blocking `wait(handle, until=…)` is **dropped**. In an
event-driven loop a blocking wait is redundant *and* worse — it makes the agent inert (can't
react to a *different* process failing while blocked) and holds a turn open for nothing.
"Wait for exit" is just the idle agent woken by the exit notification; "wait for a pattern" is
a future non-blocking `watch(handle, pattern)` (the drain task fires a notification on match)
— both compose with wake-on-notification instead of fighting it. So Slice 3 is now
idle-triggered triage.

- **Wake signal** (`src/daemon.rs`) — a `notified` `Notify` pinged by `notify()`; `inbox_wake`
  is a lost-wakeup-safe await that resolves whenever the *ephemeral pending* queue is non-empty
  (never on already-shown "left" items, so a deferred notification can't spin the loop).
- **Standalone triage** (`src/distill.rs`) — a `Distiller::triage(prefix, inbox, background)`
  with a default no-op (the stub never escalates); `ForkPeek::triage` forks off the cached
  prefix, shows the inbox + background with **no tool result**, and forces a `triage` tool
  (inbox/background dispositions, **no `keep`**). The peek-view sections and the inbox/background
  schema fields are factored out and shared with the riding `retain` gate (the Slice-1
  seam-ahead, paid off). `Triage.for_result` becomes `Option<Seq>` (`None` for standalone;
  serde reads an old bare seq as `Some`, so existing logs still load).
- **Idle loop** (`src/agent.rs`) — the idle path is now `select!{ read_user_turn, inbox_wake }`.
  An event wakes `run_idle_triage`: a 250ms debounce, drain + log first-show `Ambient`s, run the
  standalone peek, apply dispositions (pin → snapshot `Ambient` folded into the verdict; mute →
  supervisor), log a `Triage{for_result:None}`, and **escalate iff anything was pinned** — the
  contract: a pin grows the wire, so a real turn runs to act on it; nothing pinned leaves the
  projection byte-identical (audit-only), so the agent re-parks with its cache hot.

**Verified.** +2 tests (**148 green**), no new clippy warnings. `parse_dispositions` (the
`triage`-tool parse) and `inbox_wake_fires_on_pending_not_on_drained` (fires on a pending
notification, blocks on a drained one, wakes on a fresh arrival).

**Live-verified (Haiku).** Two idle scenarios. (1) Backgrounded a clean-exiting job, told to
"take no further action" → it went idle, the exit woke a standalone triage (`Triage
{for_result:None}`) which **dismissed** it → no escalation, the wire unchanged (two audit-only
log entries, projection byte-identical — cache hot while idle, the §3 payoff). (2) Backgrounded
`cargo clippy -- -D warnings` (**opaque** — the model couldn't know the outcome) with "report
back if it fails"; it confirmed it'd kicked off and went idle; clippy failed (`exited 101 after
7s`); the exit woke a standalone triage that **pinned** the failure and **escalated** to a real
turn reporting the exact lint (`src/tui/mod.rs:379`, the large `Err` variant — read live from
clippy's output). Wake → pin → escalate → act, on a real opaque task.

---

## 2026-06-26 — runaway tool result → 400; swiss-cheese defense

**Bug, live.** A turn 400'd and killed the daemon with an opaque `Anthropic API returned
400 Bad Request (no body on the streaming path)`. Root-caused from the session JSONL (not,
as first suspected, the preceding compaction — the post-fold wire was structurally identical
to the turns that succeeded before it):

- The agent ran `search "consolidate"` over the **whole cwd, including the gitignored
  `.efference/`** session logs. `fs-mcp`'s `search` hand-rolled its own `read_dir` walk with
  a hardcoded exclude list (`.git/`, `target/`, …) and was **not** gitignore-aware (unlike
  `list`, which uses `ignore::WalkBuilder`). So it grepped the append-only JSONL logs, whose
  lines each embed a full prior tool result — one matched line was **458 KB**; the raw result
  was **1.57 MB**.
- The fork peek hit its `Fallback` path (no verdict) → `Distilled { keep: All }`, so
  `render_distilled` put the whole 1.57 MB on the wire. `PEEK_LINE_CAP` bounds *lines* (201
  here) but not a pathological long line. The ~1.7 MB envelope exceeded the request limit → 400.
- The SSE error path **discarded the body**, so the real cause was invisible.

**Fixed, defense in depth (swiss cheese — every layer mitigates, plus the principled fix):**

- **`fs-mcp` `search` is now gitignore-aware** (`ignore::WalkBuilder` + `standard_filters`,
  matching `list`) — the principled fix; `.efference/` is skipped (hidden *and* gitignored).
  An explicitly named ignored file is still searched. Non-UTF-8/unreadable files are now
  skipped, not fatal.
- **`search` windows long lines** — a matched line is trimmed to ±100 bytes around the match
  (`…`-elided, char-boundary-safe); matchless context lines clamp to 200 bytes.
- **Ingestion cap (`agent::cap_tool_result`, 256 KiB)** — bounds *every* tool result before
  the fork peek, the wire, and the logged `raw` (which also breaks the recursive
  log-bloat→search-bloat loop). Over-cap → a **hard error** (not a silent truncation) naming
  the byte counts with a 1 KB head, so the model narrows the call.
- **Transport now reads the error body** (`api::Client::sse_open_error`): trillium-client
  0.9.7's `SseError: Into<Conn>` exposes the response, so a non-2xx surfaces Anthropic's JSON
  `error.message` (the prior "no body" comment was stale). Logged at `error`. Classifying
  transient statuses as retryable rather than fatal is still deferred.
- **`env_logger` in `serve`** (`RUST_LOG=efference=debug`) — request sizes, fork-peek
  fallbacks, capped results, and full API error bodies.

**Tests.** efference 153 green (cap pass-through / hard-error / char-boundary). fs-mcp 15 green
(gitignore skip, named-ignored-file still searched, binary skip, match windowing + clamp,
codepoint safety). Also fixed a long-punted `unused variable: h` warning in
`supervisor.rs` tests. Not yet live-verified against the API (the body-capture path needs a
real 400).

---

## Current state (2026-06-26)

**148 tests green, clippy clean.** The harness is **dogfooded** over the daemon
(`efference serve` + `turn` / `approve` / `attach`) and now has a **full interactive TUI**
(`efference tui`): a themed streaming transcript with a **conversational, minimalist**
render (author-differentiated turns, paired call↔result exchanges, per-tool result
renderers incl. colored diffs), an emacs-keybind composer, and in-TUI approval gates — a
session-grade client comparable to a modern coding-agent TUI. Display work is iterable
without being blind (`Screenplay` snapshot harness + `cell`/`find` color assertions +
`efference gallery`). Phase 3 Slices 0–1 are in; the **ambient-notification inbox is complete** (Slices A–C, all
live-verified: the verdict refactor + logged `Triage` audit, `POST /notify` ingestion with
`slot` supersession, and inbox-in-the-peek where the model pins/dismisses notifications at
the fork gate). **Background processes — the supervisor (§6) Slice 1 is in and live-verified:**
`bash background:true` registers a child with an ephemeral `Supervisor`, its live tail is
re-sampled into the peek each gate, pins snapshot onto the wire (reusing the notification pin
path), and exits fire an `Ambient` notification. The **TUI now shows running processes** in a
live panel above the composer (TUI-bg-1, `/state`-fed, no new endpoints). **Slice 2 is in**
(directed `list_processes`/`read_process`/`kill_process`/`check_inbox` reads, plus a §3
cache-correctness fix unifying the `tools[]` source across the loop, keepalive, and `wire`).
**Slice 3 is in** — idle-triggered triage (wake-on-notification): an idle agent woken by an
event runs a standalone tool-result-free triage and escalates to a real turn iff it pins. The
blocking `wait` was dropped as redundant. Remaining on that track: an optional `watch(handle,
pattern)` for pattern-waking, and TUI-bg-2 (smooth `/stream` live tailing). Elsewhere: the inbox's deferred refinements (`urgency` / `non_redactable`); the TUI's
Stage-2 render refinement (box → `│` connector) and remaining **TUI polish**; and the
core-harness **Slice 2** (forked compaction peek + re-read).
