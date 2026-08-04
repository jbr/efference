# Transport & streaming

This layer bridges `project(log)` and an Anthropic-Messages-compatible endpoint
(Anthropic directly, or OpenRouter's `/api/v1/messages` skin — see
[providers](#providers)): it turns a projected message list into an HTTP
request, streams the response back, and hands a finished `Completion` to the
loop — which appends it to the log and forks. Everything described here is transport-only. None of it appends to the
log, changes a projection, or perturbs the byte-stability invariants guarded in
[log-and-projection.md](log-and-projection.md).

---

## Upstream SSE

Every call to `Client::complete` POSTs to `/v1/messages` with `stream: true`.
The response arrives as a Server-Sent Events stream. `api::Assembler` folds
that stream into a single `Completion` — a pure fold, unit-testable without a
live network connection.

**Event sequence.** The assembler handles exactly these SSE event types:

| Event | What it contributes |
|---|---|
| `message_start` | model name; initial `usage` (input + cache tokens) |
| `content_block_start` | opens a `text` or `tool_use` accumulator at a given index |
| `content_block_delta` | `text_delta` appends to a text block; `input_json_delta` appends a raw JSON fragment to a tool block |
| `content_block_stop` | block boundary; no-op (finalization happens in `finish`) |
| `message_delta` | final `stop_reason`; final `usage` (merged into the `message_start` counts, per field) |
| `message_stop` | terminal; no-op |
| `ping` | keepalive; no-op |
| `error` | in-stream error (e.g. `overloaded_error` after a 200); surfaced verbatim |

**Reassembly.** `finish()` collapses the accumulated blocks into a
`Payload::Assistant { text, calls }`. Tool input is the non-trivial part: the
API splits a `tool_use` block's JSON across arbitrarily many `input_json_delta`
fragments. The assembler concatenates every raw fragment and parses the whole
once the block closes. An empty accumulator (no fragments received) produces
`{}`.

`complete()` returns one `Completion`. The loop, projection, fork, and caching
see no difference from a non-streaming call — the SSE complexity is fully
contained inside `api.rs`.

**Two invariants.**

*Loud-on-unknown.* An unrecognized event type, block type, or delta type is a
hard error, never a silent drop. The API surface is explicit about what the
assembler knows how to handle; anything outside that set fails immediately and
loudly rather than silently losing data.

*Usage merge.* Both usage-bearing events are read and folded into one `Usage` by
per-field maximum (`Usage::merge_max`); which event carries a given count is
provider-dependent, so neither can be skipped.

- Anthropic reports `input_tokens` and the `cache_*` fields in `message_start`,
  with a placeholder `output_tokens: 1` that the real count in `message_delta`
  replaces.
- OpenRouter's skin reports all four as zero or `null` in `message_start` and
  the real counts — including `cache_read_input_tokens` and
  `cache_creation_input_tokens` — in `message_delta`. Reading only
  `message_start` there yielded `in 0 · cache read 0 · cache write 0` on every
  turn, which reads as a permanently cold cache.

The merge is a maximum rather than "later event wins" because both events report
per-response *totals*, not increments: neither can legitimately report less than
the other for the same field, so a placeholder or zero from either side is
harmless. This is one code path for both providers — nothing about the usage
merge switches on `Provider`.

The `Usage` struct captures all four fields (`input_tokens`, `output_tokens`,
`cache_creation_input_tokens`, `cache_read_input_tokens`), folding both an absent
field and an explicit `null` to zero.

---

## Thinking: assembled and replayed verbatim

Extended thinking is supported end-to-end. The assembler folds streamed
`thinking` / `redacted_thinking` blocks (and their `thinking_delta` /
`signature_delta` deltas) into leading `ContentBlock::Thinking { text,
signature }` / `RedactedThinking { data }` on the assistant turn; `project`
renders them as the first wire blocks (`thinking` → `text` → `tool_use`),
signatures preserved verbatim because the API rejects a modified block on
replay. Same-model replay only ever happens (the model is locked per session),
so a signature always returns to the model that minted it.

The `thinking` **request** field rides on `tool_choice`, which makes the one
illegal pairing unrepresentable: forced `tool_choice` is incompatible with
thinking (400), so a forced choice → `disabled`, `auto` → `adaptive`
(`display: "summarized"`). The main agent turn and the fork peek both send `auto` —
the shared envelope ([redaction-fork.md](redaction-fork.md)): identical
`tool_choice` and thinking are part of what lets the peek read the agent lineage's
cache — so both may think (the fork's thinking is discarded with its throwaway
tail); the spotter still forces its `spot` tool → thinking off; the keepalive is
`disabled` independently (`max_tokens: 0` rejects thinking-on). Toggling thinking
keeps the tools+system cache tier valid (see
[log-and-projection.md](log-and-projection.md)), and it's set once per
call-type and never flipped turn-to-turn, so it stays out of the cached
prefix's way. The wire encoding is pinned by
`project::tests::thinking_turn_matches_expected_wire_bytes`. Thinking blocks are
never elided today — that's a deferred generalization of the redaction model
(the log holds them all verbatim).

---

## HTTP/2 over trillium-rustls

`Client` uses `trillium-client` with `trillium-rustls` for TLS. ALPN
negotiates HTTP/2 — Anthropic is fronted by Cloudflare, which does not
advertise `alt-svc: h3`, so HTTP/3 is not a live option. HTTP/2 is the path.

## Transient upstream failures

`Client` installs `trillium-client-retry`'s `RetryHandler` around every upstream
request. The handler retries the replay-safe Messages POSTs (their serialized
request body is an owned `String`) on 429, 500, 502, 503, and 504. Other 4xx
responses remain terminal, so malformed or oversized requests fail without
replaying them. It makes at most four total attempts and has a 30-second
wall-clock budget; an HTTP `Retry-After` is honored only within that same
budget.

The retry handler covers failures while opening a response, before an SSE stream
has yielded any deltas. A connection that has returned 200 and then drops
mid-stream is not resumed: it may already have published live deltas and left a
partially assembled completion.

An exhausted `complete` request fails the current trajectory rather than the
daemon. No assistant payload is appended, so the projection remains unchanged;
the failure is written to the daemon's stderr and the loop returns to idle,
where the operator can submit another turn or change session configuration.
Keepalive failures are likewise transport-only and leave the log unchanged.

Every non-2xx response also updates one shared diagnostic: its numeric status and
how many consecutive responses carried that status. `RetryHandler` is outermost
and the observer is inside it, so reverse response handling records each failed
attempt before the retry handler queues its follow-up. The daemon exposes this
optional value as `upstream_failure` on `GET /state`; the TUI polls it and shows
`upstream 429 ×2` in its status bar. It remains after a later success: there is
no transport lifecycle boundary chosen as an all-clear event.

---


## Providers

The request **body** is always the Anthropic Messages wire — `project(log)`
output — so the entire log, projection, assembler, and cache-breakpoint
model are provider-independent. What varies between providers is **where** the
POST goes, **how** it authenticates, and **which metadata endpoint** answers
questions about the selected model. All three derive from one declared
identity: `api::Provider`, a closed enum (`anthropic`, `openrouter`) set by
`EFFERENCE_PROVIDER` and grown in code as providers are added. Identity is an
input, never inferred from URL shape (sniffing fails for proxies and custom
URLs). `api::Endpoint` carries the derived `url` + `auth_header` + `auth_value`
alongside the provider, resolved once at boot and shared by the agent `Client`
and the fork's spotter client so both hit the same endpoint over the same
cached prefix.

Env knobs:

- `EFFERENCE_PROVIDER` — `anthropic` (the default when unset) or `openrouter`.
  An unrecognized value is a hard error naming the valid ones, never a silent
  fallback to a host the key doesn't belong to.
- `EFFERENCE_API_KEY` — the **raw secret**, and nothing else. The auth header
  name and any scheme prefix come from the provider (`x-api-key` for Anthropic;
  `authorization` + `"Bearer "` for OpenRouter), so the secret stays a **whole
  value** — a 1Password reference that `op run` resolves (`op run` only
  substitutes an `op://…` reference that is the entire value, never one
  embedded mid-string).
- `ANTHROPIC_API_KEY` — fallback secret for the Anthropic provider only. Under
  `openrouter` it is rejected at resolve with a message naming
  `EFFERENCE_API_KEY`: an Anthropic key is the wrong credential there, and the
  early error beats a confusing 401.

An earlier design assembled the endpoint from data instead — URL, header name,
and prefix each their own env knob, with no provider identity in the code. It
was replaced because the provider knowledge didn't disappear, it moved into the
operator's head (three knobs to reach OpenRouter), and because every
model-conditional decision (the context ceiling first, more behind it) was
unanswerable without knowing which metadata endpoint to ask. Those knobs
(`EFFERENCE_ENDPOINT`, `EFFERENCE_AUTH_HEADER`, `EFFERENCE_AUTH_PREFIX`) are
now hard errors naming the replacement — silently ignoring a set one would
post somewhere other than where the operator's environment says.

**The boot capability query.** `serve` fetches the selected model's metadata
from the provider before the daemon starts (`Endpoint::fetch_capabilities`,
`api::provider`), and a failed lookup — network, non-2xx, missing fields —
**aborts startup** with the URL and body in the message. The query is required
rather than best-effort because capability conditionals hang off the result
and "unknown" would silently disable them; there is no offline mode to
regress (serve already requires the live messages endpoint). Per provider:

- `anthropic`: `GET /v1/models/{model}` — `max_input_tokens` is the context
  window, `max_tokens` the output cap.
- `openrouter`: `GET /api/v1/models/{author}/{slug}/endpoints` — one entry per
  upstream currently serving the model, each with its own `context_length` /
  `max_completion_tokens`. Routing is unpinned, so the effective ceiling is
  the **min** across endpoints until provider pinning lands.

The parsed `ModelCapabilities` (`context_length`, `max_output_tokens`; grown
one field per consumer) is transport-only — held in `Shared`, never logged or
projected. `GET /state` surfaces it as `usage.context = {limit, used}`, where
`used` is the last request's total prompt tokens (`input_tokens` +
`cache_read_input_tokens` + `cache_creation_input_tokens` — `input_tokens`
alone is only the uncached remainder; null until the first request). The TUI
sidebar renders it as the `ctx 7% · 14k/200k` line leading STATS.

**Session cost telemetry.** Where the provider has a per-request accounting
surface, every recorded request (all four origins — turn, peek, spot,
keepalive — plus keepalive pings funnel through the same two record methods)
spawns a best-effort fetch of its **billed** cost, folded into a running total
that `GET /state` reports as `usage.cost = {usd, unpriced}` and the sidebar
renders as `cost $0.0421`. Cost is fetched, never modeled from a rate card —
cache read/write/TTL multipliers and per-endpoint price variance make modeling
brittle. OpenRouter is the one provider with such a surface today:
`GET /api/v1/generation?id=…` returns `total_cost`, keyed by the generation id
each response carries (the same id `usage.jsonl` records — the join to the
provider's own accounting). The record lands shortly after the response
completes and a too-early GET 404s, so the fetch makes two delayed attempts
(2s, 10s); exhaustion increments `unpriced`, so the total is a legible
undercount rather than a silent one. On direct Anthropic there is no
per-request cost surface (the Models API has no pricing; the Usage & Cost
Admin API reports realized org costs in daily buckets under a separate admin
key) — `usage.cost` is null there and the sidebar omits the line.

**OpenRouter** is reached by declaring it: `EFFERENCE_PROVIDER=openrouter`,
`EFFERENCE_API_KEY=sk-or-…`, and a provider-scoped model slug
(`anthropic/claude-haiku-4.5` — the `author/slug` shape is also what the
metadata route takes as path segments; a slash-less model id fails the boot
lookup before any request). Its `/api/v1/messages` skin is the Anthropic
Messages wire: it honors per-block `cache_control` breakpoints (across all
Anthropic-compatible providers), emits Anthropic-shaped SSE `error` events, and
its `: OPENROUTER PROCESSING` keepalive comments are skipped by the SSE decoder
(leading-colon lines are comments). The one Anthropic-dialect header efference
sends besides auth, `anthropic-version`, is part of the *wire dialect*, not a
provider identity — OpenRouter tolerates it (on the metadata GET too) — so it
stays a constant.

**Caching portability.** Only Anthropic's *explicit* `cache_control` breakpoints
are Anthropic-specific; every other major model does **automatic prefix
caching** (OpenAI, Gemini's implicit caching, DeepSeek). The byte-deterministic,
append-only-prefix discipline the projection enforces is exactly what those
automatic caches reward — foreign providers simply ignore the `cache_control`
blocks (or translate them, as OpenRouter does for OpenAI). What's lost off the
Anthropic path is control over breakpoint *placement* and the `1h` TTL; the short
foreign TTLs keep the keepalive relevant, though whether a `max_tokens: 0` ping
refreshes a non-Anthropic cache is unverified.

**Thinking.** Adaptive thinking is an Anthropic `thinking` block most foreign
models don't accept, so `supports_adaptive_thinking` is an **allowlist** (a
recognized non-Haiku Claude tier), not "everything but Haiku" — a foreign model
gets no `thinking` block by default. `EFFERENCE_THINKING=adaptive|off|auto`
overrides per session (there is no portable capability query: OpenRouter's
`supported_parameters` describes its normalized `reasoning` param, not the
Anthropic `thinking` block).

That metadata stays out of `supports_adaptive_thinking` even though a boot-time
capability query now exists (above) and OpenRouter's `reasoning.supported_efforts`
field happens to track the adaptive-capable Claude generation cleanly (present on
4.6+, absent on haiku-4.5 and the 4.5/4.1 tier). The signal is invalid, not just
inconvenient: it describes OpenRouter's *normalized* interface, not the block
efference sends — proof: `thinkingmachines/inkling` advertises `supported_efforts`
but rejects an Anthropic `thinking` block, so the field is strictly broader than
"accepts `thinking:{type:adaptive}`" and cannot replace the Claude-family gate.
It stays useful offline, as a reference for setting `EFFERENCE_THINKING` by
hand. Whether acceptance can instead be determined from the response JSON
itself (a probe, or first-turn observation) is an open question parked in the
backlog.

---

## Token-level deltas — `GET /stream`

Assistant prose and tool stdout are the same thing: ephemeral, pre-commit
output that the committed log entry supersedes once the turn closes. One
mechanism carries both.

`Shared` holds a **lossy** `broadcast::Sender<StreamDelta>`. `StreamDelta` has
two variants:

- `AssistantText(String)` — a prose fragment from a `text_delta` event
- `ToolOutput { call_id, chunk }` — a chunk from a running tool's stdout or
  stderr

`GET /stream` subscribes per connection and receives deltas as they arrive.

**Deliberately lossy.** A lagging consumer drops deltas rather than stalling
the producing loop. The log is the authoritative backstop; ephemeral deltas are
only a live view. Where `/events` is durable (with resume cursors), `/stream`
is best-effort.

**ID-less by design.** Delta events carry no SSE `id:` field. That absence is
the contract that keeps them off the `/events` resume cursor. `StreamDelta` is
never appended to the log, never projected, and carries no sequence number —
it is transport-only.

**Producer ignorance.** `api.rs` and `bash.rs` each take a plain
`impl FnMut(&str)` sink. Only the agent loop adapts those callbacks into
`StreamDelta` and publishes to the broadcast channel. Producers have no
knowledge of the daemon or the channel.

**The fork peek passes a no-op sink.** The fork's triage tokens are internal
bookkeeping; they must never surface on the stream. The no-op `|_| {}` is the
enforcement.

**Bash streams both pipes, arrival-interleaved.** `bash.rs` drives concurrent
`tokio::select` reads of stdout and stderr, which is terminal-faithful (cargo
build progress, test output, etc.) and preserves the process-group abort path.

**`attach` merges `/events` + `/stream` in one render task.** Live fragments
print first; the committed entry finalizes the line when the turn closes. Tool
output is intentionally not deduped against the committed `tool_result`: the
stream shows raw live output, the result shows the fork's distillate (see
[redaction-fork.md](redaction-fork.md)). Both carry signal.

---

## Cache instrument panel and keepalive

Caching shapes this layer (see [log-and-projection.md](log-and-projection.md) for the
TTL-is-a-write-time-property background). It
was previously invisible and died at the 5-minute TTL on any idle. This layer
makes it legible and keeps it warm, entirely within transport and daemon — no
log appends, no projection changes, fully reversible.

### max_tokens: 0 as the keepalive primitive

A `max_tokens: 0` request reads (and thus refreshes) the cached prefix.
`cache_read_input_tokens > 0`, `output_tokens: 0`, instant. The empty assistant
turn is discarded; the log is never touched.

This call is non-streaming by design: `max_tokens: 0` is rejected by the API
when paired with `stream: true`.

The prefix bytes sent by a keepalive must be byte-identical to what a real turn
sends, or the ping writes a new cache entry instead of refreshing the live one.
`Client::keepalive` takes the same `messages`, `tools`, and `tool_choice` the
loop uses, and reuses the same `system` prompt from the client, so identity is
structural rather than a fragile string comparison.

### Metrics cell

`Shared` holds a transport-only metrics cell: the latest `Usage`, cumulative
totals, two monotonic `Instant`s, and one wall-clock stamp. Two clocks, two
jobs: the `Instant`s drive *interval* logic (when to ping, when the keepalive cap
is reached), where a monotonic clock is correct because an NTP step must not move
a deadline; the wall stamp is what clients read. Neither reaches the projection.
`GET /state` carries a `usage` block with `last`, `cumulative`, and
`last_request_at` — an **absolute** instant rather than an elapsed count, because
a field that ticks with the clock would make the level-triggered `usage` topic
re-emit forever (docs/control-api.md). Clients subtract from their own clock.

The fork peek's usage is folded into the metrics cell because the fork reads the
shared canonical prefix and therefore slides the cache TTL.

### keepalive_loop: two clocks

`keepalive_loop` is spawned in `serve`. It fires `Client::keepalive` when the
cache nears eviction at idle.

Two clocks keep this correct:

- **Cache-touch clock** — stamped by `record_request` (real turns and the fork
  peek) and by `record_keepalive`. Tracks when the cache was last read.
- **Real-turn clock** — stamped only by `record_request`. Tracks when a real
  turn last occurred.

A keepalive refreshes the TTL (`record_keepalive` stamps the touch clock) but
does not reset the give-up deadline (the real-turn clock is untouched). This
prevents a keepalive loop from keeping itself alive forever.

The loop pings when the touch clock exceeds `PING_AT` (240 seconds — a margin
under the 5-minute TTL) and stops once the real-turn clock exceeds `CAP` (30
minutes of genuine idle). Past that threshold, one cold cache read on resume
is cheaper than indefinite pinging. During active work, real turns keep the
touch clock reset and the keepalive never fires.
