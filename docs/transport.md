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
| `message_delta` | final `stop_reason`; final `output_tokens` (overwrites the placeholder from `message_start`) |
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

*Usage merge.* `input_tokens` and the `cache_*` fields arrive in
`message_start`; the final `output_tokens` arrives in `message_delta`. Both
events must be read and merged. Reading only `message_start` regresses the
cache-gate metrics — `message_start` carries a placeholder `output_tokens: 1`
that `message_delta` overwrites with the real count. The `Usage` struct captures
all four fields (`input_tokens`, `output_tokens`, `cache_creation_input_tokens`,
`cache_read_input_tokens`), defaulting the cache fields to zero when absent.

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
(`display: "summarized"`). The main agent turn is `auto` → thinks; the fork
forces its distiller → thinking off; the keepalive is `disabled` independently
(`max_tokens: 0` rejects thinking-on). Toggling thinking keeps the tools+system
cache tier valid (see [log-and-projection.md](log-and-projection.md)), and it's set once per
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

---

## Providers

The request **body** is always the Anthropic Messages wire — `project(log)`
output — so the entire log, projection, assembler, and cache-breakpoint
model are provider-independent. What varies between providers is only **where**
the POST goes and **how** it authenticates. That is the whole of `api::Endpoint`
(`url` + `auth_header` + `auth_value`), resolved once at boot from the
environment and shared by the agent `Client` and the fork's spotter client so
both hit the same endpoint over the same cached prefix.

Env knobs, all optional so a pre-existing `ANTHROPIC_API_KEY`-only setup is
unchanged:

- `EFFERENCE_ENDPOINT` — the full messages URL (default: Anthropic's).
- `EFFERENCE_API_KEY` — the **raw secret**, and nothing else.
- `EFFERENCE_AUTH_HEADER` — the auth header *name* (default `x-api-key`).
- `EFFERENCE_AUTH_PREFIX` — a literal prepended to the secret to form the header
  value (default empty; `"Bearer "` for OpenRouter's `authorization: Bearer …`).

Header name and scheme prefix are split *off* the secret deliberately: it keeps
the secret a **whole value**, so it can be a 1Password reference that `op run`
resolves (`op run` only substitutes an `op://…` reference that is the entire
value, never one embedded mid-string). Provider identity stays entirely in the
environment — `x-api-key` + no prefix (Anthropic) and `authorization` +
`"Bearer "` (OpenRouter) are the same code path, differing only in data. The
`EFFERENCE_AUTH_*` knobs apply only to the `EFFERENCE_API_KEY` path; a bare
`ANTHROPIC_API_KEY` is always sent as `x-api-key`.

**OpenRouter** is reached this way with no code change: `EFFERENCE_ENDPOINT` =
`https://openrouter.ai/api/v1/messages`, `EFFERENCE_AUTH_HEADER=authorization`,
`EFFERENCE_AUTH_PREFIX="Bearer "`, `EFFERENCE_API_KEY=sk-or-…`, with a
provider-scoped model slug (`anthropic/claude-opus-4.1`). Its `/api/v1/messages`
skin is the Anthropic
Messages wire: it honors per-block `cache_control` breakpoints (across all
Anthropic-compatible providers), emits Anthropic-shaped SSE `error` events, and
its `: OPENROUTER PROCESSING` keepalive comments are skipped by the SSE decoder
(leading-colon lines are comments). The one Anthropic-dialect header efference
sends besides auth, `anthropic-version`, is part of the *wire dialect*, not a
provider identity — OpenRouter tolerates it — so it stays a constant.

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
totals, and two wall-clock `Instant`s. The `Instant`s are never serialized —
only derived elapsed seconds reach a client — so their non-determinism cannot
escape into the projection. `GET /state` carries a `usage` block with `last`,
`cumulative`, and `seconds_since_last_request`.

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
