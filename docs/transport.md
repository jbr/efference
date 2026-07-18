# Transport & streaming

This layer bridges `project(log)` and the Anthropic Messages API: it turns a
projected message list into an HTTP request, streams the response back, and
hands a finished `Completion` to the loop — which appends it to the log and
forks. Everything described here is transport-only. None of it appends to the
log, changes a projection, or perturbs the byte-stability invariants guarded in
[the substrate](substrate.md).

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

**Two invariants, both load-bearing.**

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
cache tier valid (see [the substrate](substrate.md)), and it's set once per
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

Caching is the constraint everything bends around (see [the
substrate](substrate.md) for the TTL-is-a-write-time-property background). It
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
