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
| `message_stop` | terminal; marks the stream **finalized** (as does OpenRouter's `[DONE]` sentinel) |
| `ping` | keepalive; no-op |
| `error` | in-stream error (e.g. `overloaded_error` after a 200); stops assembly, body preserved as the completion's `upstream_failure` |

**Reassembly.** `finish()` collapses the accumulated blocks into a
`Payload::Assistant { text, calls }`. Tool input is the non-trivial part: the
API splits a `tool_use` block's JSON across arbitrarily many `input_json_delta`
fragments. The assembler concatenates every raw fragment and parses the whole
once the block closes. An empty accumulator (no fragments received) produces
`{}`.

`complete()` returns one `Completion`. The loop, projection, fork, and caching
see no difference from a non-streaming call — the SSE complexity is fully
contained inside `api.rs`.

**Three invariants.**

*Loud-on-unknown.* An unrecognized event type, block type, or delta type is a
hard error, never a silent drop. The API surface is explicit about what the
assembler knows how to handle; anything outside that set stops assembly
immediately and loudly rather than silently losing data — the refusal becomes
the completion's `upstream_failure` (below), so it survives to the log rather
than only to stderr.

*Finalized-or-marked.* The assembler tracks whether the stream **finalized**
(`message_stop` observed, or `[DONE]`). A stream that dies without finalizing —
a silent close, an in-stream `error` event, a mid-stream transport error, an
event the assembler refused — still assembles, but the completion's payload
carries `upstream_failure: Some(why)`: a **record-only partial**
([log-and-projection.md](log-and-projection.md)). The partial content is
preserved (it is often the only copy anywhere; a cut tool input is kept as its
raw string rather than erroring the whole partial away), but the provider has
no record of the generation and doesn't stand behind it, so the entry never
projects. The dividing line from the max-tokens case is deliberate: a
cap-terminated response is *finalized* — it carries a `stop_reason` and usage
and the loop rightly commits it — where an unfinalized stream carries neither.
Finalized → commit; unfinalized → record-but-retry. A failure observed *after*
finalization (draining the already-closed stream) does not unmake the
completion. Before this existed, an aborted stream committed as a clean turn —
empty or thinking-only, zero usage — indistinguishable from the agent choosing
silence (observed three times in one session, 2026-08-21; OpenRouter 404'd all
three generation ids).

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

The `thinking` **request** field rides on `tool_choice`, the model family, and
the boot capability lookup (`Thinking::for_request`). On Claude the one illegal
pairing is unrepresentable: forced `tool_choice` is incompatible with thinking
(400), so a forced choice → `disabled`, `auto` → `adaptive`
(`display: "summarized"`) on the recognized tiers (not Haiku). **Foreign models
never get `disabled`** — a mandatory-reasoning endpoint rejects an explicit
disable outright (probed live, 2026-08-21) — they get `adaptive` on `auto` when
the metadata grants `reasoning` (the foreign-provider section below), and the
field **omitted** otherwise, forced choice included: probed, a
mandatory-reasoning endpoint thinks and then honors the forced call. The main
agent turn and the fork peek both send `auto` —
the shared envelope ([redaction-fork.md](redaction-fork.md)): identical
`tool_choice` and thinking are part of what lets the peek read the agent lineage's
cache — so both may think (the fork's thinking is discarded with its throwaway
tail); the spotter still forces its `spot` tool → thinking off (`disabled` on
Claude, omitted foreign); the keepalive follows the same family split
independently (`max_tokens: 0` rejects thinking-on). Toggling thinking
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
has yielded any deltas. A connection that has returned 200 and then dies
mid-stream is not resumed — it may already have published live deltas — but it
is not lost either: the partial assembles as a record-only completion
(finalized-or-marked, above), and the **agent loop retries the whole request**.
The loop makes up to `STREAM_ABORT_ATTEMPTS` (12) attempts per model call with a
doubling pause capped at 30s — about three and a half minutes of waiting in all;
each aborted attempt's partial is appended to the log first. The budget is sized
to **wait the failure out**, because a mid-stream abort is an ephemeral upstream
condition and the alternative on exhaustion is worse than waiting: idling with
"send a turn to retry" needs an operator, and an autonomous run has none. The cap
is what lets the count be large without the tail becoming a hang — uncapped
doubling over twelve attempts would sleep for over an hour. Because a marked entry projects to nothing, every retry re-sends
byte-identical messages and rides the still-warm cached prefix. On exhaustion
the loop posts a notification naming the failure (the same pattern as the
output-token-cap notification) and returns to idle — the operator's view and
the agent's next triage gate both see that the turn was not completed, rather
than a silence indistinguishable from the model producing nothing.

That idle is **legible as a stall**, not only as an idle. The daemon stays up on
this path deliberately — the socket is alive and one turn recovers the session —
but the return to idle otherwise looks exactly like a finished run to a client:
`running: false`, no gate, and an inbox that a triage peek may already have
cleared (observed 2026-08-28, where the peek dismissed the recovery notice and
left a clean idle). So the loop also sets the **`stall` state topic**
(control-api.md): the reason, the attempts spent, the instant it gave up, a lead
line naming what recovers it, and the upstream's words as `detail`. It is
cleared at the top of the next trajectory rather than on a successful
completion, so it reports the condition now. The notification is addressed to
the agent's next triage gate; the topic is the copy addressed to whatever is
supervising the daemon from outside, which is the rung that can actually send
the recovering turn. Both clients render it where turn state already shows — the
TUI's status bar, the web composer's turn-state readout.

The **fork peek** takes the same treatment on the same budget: an aborted peek
stream is retried up to `STREAM_ABORT_ATTEMPTS` times on the same capped
schedule, re-sending byte-identical messages over the warm prefix (its
tail is throwaway, so nothing has to be committed to arrange the replay). Only
on exhaustion does the gate withhold. The surfaces used to disagree — an
aborted turn got four attempts while an aborted peek withheld on the first —
and the asymmetry was expensive in a way the turn path is not: a withheld
result asks the agent to re-issue a call whose side effects already stand. In a
controlled run a withheld conformance `PASS` was re-earned by re-running the
suite against a still-live server whose counters the invisible first run had
moved, turning the pass into a spurious failure.

### The duration ceiling

Everything above bounds *failures*. A stream that stays open and keeps trickling
— or stops trickling without closing — is bounded by none of it: the retry
handler's 30-second budget covers only the attempt to *open* a response, and a
response that is open has no other clock on it. Left alone, such a stream blocks
its turn indefinitely and shows nowhere, since nothing aborts and `upstream_retry`
only reports a retry already in flight.

So the read loop in `Client::complete` carries an absolute deadline
(`tokio::time::timeout_at` on each `events.next()`). Exceeding it stops the read
and marks the stream through the same `Assembler::fail` a mid-stream death takes:
the partial is kept as a record-only entry, the reason rides on the completion's
`upstream_failure`, and the caller's abort-retry budget decides what happens next
— which means it is retried on the schedule above, publishes `upstream_retry`
while it waits, and lands on `stall` if the budget runs out. It is never a silent
kill. Because `complete` is the one read loop, **both** stream surfaces are
bounded by it: a turn and a fork peek alike.

The value is operator-set — `[transport] stream-ceiling`, in seconds, alongside
`[bash]`'s knobs (`config::TransportLimits`) — because the right one is
workload-dependent: a long think and a hung gateway look identical from outside
until one of them ends. The default is **one hour**. The widest model call in
this repo's corpus (2026-08-28, 2281 assistant entries) took 363 seconds and the
99.9th percentile 253, so an hour is about ten times the longest wait anyone here
has actually sat through. It is set that far out on purpose: hours-scale thinking
is a direction models are moving, a ceiling that fires on a merely *slow* stream
discards real work, and one that fires late only delays the diagnosis of a hang.
The counterpart to a generous value is that a genuinely hung endpoint costs the
ceiling once per abort-retry attempt before the trajectory stalls — which is the
other reason the number belongs to whoever knows the workload.

This was never observed directly; the empty-finalized-turn cases were *consistent
with* a gateway timing out in front of a slow upstream, and the ceiling closes the
adjacent hole rather than a measured one.

A stream that **finalizes with nothing** — proper termination, zero content
blocks, zero usage — is the complement of one that dies before finalizing, and is
marked the same way. It does carry a generation id: a `message_start` frame
arrives and sets it, so the shape on the wire is that frame followed directly by
the terminator. A gateway in front of a slow upstream can
answer this way instead of erroring; committed as-is it reads as the model
choosing silence, so the loop has nothing to dispatch and goes idle with the task
unfinished. Because the mark *is* the record-only/retryable signal, marking it in
the assembler is what makes both surfaces handle it: the turn loop and the fork
peek each retry on the budget they already have. Zero output tokens is required
as corroboration, so a turn billed for the thinking behind an empty answer is not
mistaken for this and retried into exhaustion. Observed 2026-08-22 and again
2026-08-28, the second time ending a controlled run mid-task, and three more
times in a controlled run later that day — those three all recovered on the next
attempt.

**Which leg produces it is not currently answerable.** The event survives as one
prose string on a record-only entry, and the generation ids it carries 404
permanently at OpenRouter on both `GET /generation` and `GET /generation/content`
(the latter is what would carry `previous_errors[]` with each upstream attempt's
provider and raw body). So an empty stream cannot be attributed between the
gateway and the provider behind it after the fact. The status, the response
headers, and the raw frames are all discarded at a seam that keeps them for a
non-2xx — `UpstreamStatusHandler::after_response` returns early on success, and
an empty stream is a 200. What to capture instead is
backlog/empty-stream-diagnostics.md.

An exhausted *open* failure (a non-2xx after the retry handler gives up) still
fails the current trajectory rather than the daemon, but no longer only to
stderr: the loop appends an empty record-only assistant entry carrying the
error body — Anthropic's 4xx bodies name exactly what was malformed, and the
log is the durable file where stderr is not — and posts the same
turn-not-completed notification, and sets `stall` with `reason:
"request_failed"`, before returning to idle. The projection
remains unchanged either way. Classifying *which* open failures deserve further
loop-level retries beyond the transport's own remains deferred. Keepalive
failures are transport-only and leave the log unchanged.

Every non-2xx response also updates one shared diagnostic: its numeric status and
how many consecutive responses carried that status. `RetryHandler` is outermost
and the observer is inside it, so reverse response handling records each failed
attempt before the retry handler queues its follow-up. The daemon exposes this
optional value as `upstream_failure` on `GET /state`; the TUI shows
`upstream 429 ×2` in its status bar. It is **cleared by the next 2xx**: a
response that arrives is the all-clear, which also covers the retrying case,
since an attempt cannot finalize without one. The chip therefore reports a
failure that is current rather than one that happened once and was survived —
a chip that persists through a full recovery teaches an operator to ignore the
one surface meant to carry signal. It stays content-free: the status and the
count are all it carries, and the bodies live in the log (below).

### The retry in flight

While either loop is waiting out an aborted stream, that wait is its own
level-triggered state topic, `upstream_retry` on `GET /state`
(docs/control-api.md). It names the **surface** (`turn` or `peek`), the attempt
that just failed, the budget it is spending, the absolute instant the next
attempt fires, a lead line — *stream aborted, retrying (2 of 12)* — and the
upstream's own error text as `detail`. Both retry loops publish it (`agent.rs`
for a turn, `distill.rs` for a fork peek) through one shared constructor, so the
surfaces differ only in the name they carry; the distinction is kept because an
exhausted peek withholds a verdict where an exhausted turn ends the trajectory.
It is set when an abort begins a pause, replaced per attempt, and cleared by the
attempt that finalizes or by exhaustion — nothing is being retried once the
budget is spent, so the topic does not double as a gravestone; `stall` is the
topic that outlives the failure. Without it, a generation on its third
attempt is indistinguishable from one that is taking a while, and an operator
watching a live autonomous run cannot tell "the provider is flaky and the
harness is absorbing it" from "the model is thinking hard" — which call for
different responses. The retry cell carries its own change signal rather than
relying on the daemon's wake-on-append fallback: a pause has no append to ride
on and lasts up to thirty seconds, so a late topic would be wrong for the entire
time the state is true. Rendering it is the clients' half; the API carries it
either way.

### A transient failure's record is not transient

Each **failed HTTP attempt** is recorded durably, bearing its status and body, as
a record-only assistant entry that projects to nothing — the same class as an
aborted attempt's partial. That covers the attempts the retry handler absorbs: a
429 that recovers on the next try used to leave nothing behind but a status and a
count, so the chip could not be inspected for what actually went wrong. The
handler sits below the loop and cannot append, so the body crosses that boundary
through an in-memory queue beside the failure cell: the observer reads the
non-2xx body once, stashes it on the conn for whoever asks next (the SSE open
error, the keepalive), and queues a copy; the agent loop drains the queue into
log appends after each model call. The queue is bounded and drops its oldest
entries, so an outage against a daemon that is not draining cannot grow the
process without bound — the durable record is the log, not the queue.

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

**Sticky routing (`x-session-id`).** OpenRouter routes a model slug to *some*
upstream provider and keeps a conversation on one of them so the prompt cache
stays warm. Absent an explicit key it derives one by hashing the first system
message and the first non-system message — which `consolidate` moves: it elides
every entry up to `upto`, including the original first user message, and projects
a summary in its place ([consolidate.md](consolidate.md)). The first non-system
message is therefore different on the next turn, the derived key changes, and
OpenRouter is free to route elsewhere — reading a cache write back against an
upstream that never saw it. Sending the key explicitly is what pins it.

The value is `<session id>-<hash of the session dir's canonical path>`, derived
once at boot by `sticky_session_id` in `src/api/endpoint.rs`. Session ids are
unique per `.efference/` dir, not per account, so two repos both served
`--name work` would otherwise collide; the hash separates them while the
human-chosen name stays readable in OpenRouter's Sessions view, the other thing
the key buys. The path is canonicalized through the parent, since the session dir
does not exist yet on the first turn and a fresh session must key the same as a
resumed one. Capped at 256 characters (OpenRouter's limit) by truncating the
*name* — `--name` is arbitrary user input, and a cap that ate the hash instead
would defeat the disambiguation.

Which requests carry it is the **provider's** call: `Provider::session_header`
returns the header name, or `None` where the concept doesn't exist. Anthropic
returns `None` — nothing in Messages selects an upstream, because there is only
one, and `metadata.user_id` is an abuse-detection identifier rather than a
routing key. An earlier draft sent the header unconditionally on the grounds that
an unrecognized header is free; gating on the declared provider means that
assumption never has to hold. Offline verbs (the `spotter` probe, `wire`) derive
no key at all — a one-shot probe has no conversation to pin.

The same value also rides the **request body** as `prompt_cache_key`, on the
providers that read it (`Provider::prompt_cache_key_field` — OpenRouter only;
Anthropic's Messages API has no such field and 400s an unrecognized top-level
key, so this cannot be sent unconditionally the way a header could). It is
deliberately redundant with the header, and third in OpenRouter's own precedence
order — body `session_id`, then `x-session-id`, then `prompt_cache_key`
(verified against their prompt-caching docs, 2026-08-28). It is in the body
because a header is the part of a request an intermediary may drop, and because
an upstream that speaks the OpenAI dialect keys *its own* prompt cache on this
field rather than on OpenRouter's routing key. Like `model` and the upstream pin
it is a per-session constant, so it cannot toggle mid-session; unset, it skips
serialization entirely, so adding it moved no Anthropic session's bytes.

The spotter takes a **distinct** key (`Endpoint::for_spotter`, a `-spot` suffix).
It has its own system prompt and shares no prefix with the agent turn, so the two
are separate conversations over one endpoint; message-hash derivation separated
them for free, and an explicit key overrides that, so the separation has to be
re-stated. Nothing stops an operator pointing both clients at the same model, and
a shared key would then pin two unrelated prefixes to one route.

Byte-stability is the point: the key is derived from the session dir, which is
fixed for the session's life, and must not move when the projection does. The
hash is a spelled-out FNV-1a rather than `DefaultHasher`, whose output is
explicitly not stable across Rust releases — a rebuild mid-session would
otherwise move the key.

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
  `max_completion_tokens`. Routing is unpinned, and the two ceilings aggregate
  in **opposite** directions because they are used differently.
  `context_length` takes the **max**: OpenRouter routes only to endpoints that
  fit the request, so a prompt wider than the narrowest endpoint narrows the
  routable set rather than failing, and a min under-reports by the width of the
  narrowest outlier (`z-ai/glm-5.3-flash`, 2026-08-26: seven endpoints at 1M or
  more, one at 256k — the min reported 256k for a 1M model).
  `max_completion_tokens` takes the **min**: it is sent as the request's
  `max_tokens`, and routing skips endpoints that cannot produce a response that
  long, so a max would silently pin routing to the single widest endpoint and
  change price and availability as a side effect of a display value. The
  model-level fields are not used: `context_length` there is the max across
  endpoints, while `top_provider` describes one designated endpoint (not the
  originating lab, and not necessarily the cheapest), and the two disagree.

When `--upstream` pins the routing (below), the pin **narrows the endpoint set
before anything aggregates over it**: a pinned session routes to exactly those
endpoints, so a ceiling taken across the ones it will never reach is someone
else's. A pin naming no endpoint in the listing falls back to the whole set
rather than emptying it — that request fails loudly on its own, and an empty set
here would abort boot with a message about `context_length` that names the wrong
problem.

The parsed `ModelCapabilities` (`context_length`, `max_output_tokens`,
`supports_reasoning`, `supports_image_input`, `supports_prompt_caching`; grown
one field per consumer) is transport-only — held
in `Shared`, never logged or projected. `max_output_tokens` sets the
per-turn output budget (`Client::with_max_output`): a reasoning model's
thinking bills against `max_tokens`, and the pre-reasoning 4096 default —
which stands only when the provider reports no cap — starves a long think,
terminating the turn at the cap before any text or call arrives (observed
live, 2026-08-21; the committed turn was thinking alone). A cap-terminated
turn commits whatever arrived and posts a notification naming the cap
(`agent.rs` on `stop_reason: "max_tokens"`) — before that existed, the
session just sat idle, reading as a hang. `GET /state` surfaces it as `usage.context = {limit, used}`, where
`used` is the last request's total prompt tokens (`input_tokens` +
`cache_read_input_tokens` + `cache_creation_input_tokens` — `input_tokens`
alone is only the uncached remainder; null until the first request). The TUI
sidebar renders it as the `ctx 7% · 14k/200k` line leading STATS.

**Cache-read telemetry.** The same `/state` topic carries `usage.cold_streak`:
consecutive main-loop requests that read nothing from the prompt cache and had
no reason not to (not session boot, not a model change, not idle past the TTL).
It is the live end of the empirical prefix oracle — the byte pins prove the
harness emits what it emitted yesterday, and only a read rate proves the
provider still matches it. The classifier, the threshold and the corpus it came
from, and the offline `efference cache` view are in
[log-and-projection](log-and-projection.md).

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
Anthropic path is control over breakpoint *placement*
([log-and-projection.md](log-and-projection.md) — the placement is
provider-conditional and the portable one leaves a trailing tool suffix unmarked)
and the `1h` TTL; the short foreign TTLs keep the keepalive relevant, though
whether a `max_tokens: 0` ping refreshes a non-Anthropic cache is unverified —
which is why the keepalive is gated on the model reporting a cache at all and
capped by the runtime tripwire.

**Thinking.** A foreign model's `auto`-turn adaptive is granted by the
capability lookup: `ModelCapabilities::supports_reasoning` — conservative like
`max_output_tokens` and not like `context_length`, since `thinking` is not a
routing filter: every endpoint currently serving the model must list
`reasoning` in `supported_parameters` — sends the same `thinking:{type:adaptive}` block, which
OpenRouter's compat layer translates to its normalized `reasoning`. A foreign
model not granted it gets no `thinking` field at all — omission, never
`disabled`, which mandatory-reasoning endpoints reject outright.
`EFFERENCE_THINKING=adaptive|off|auto` still overrides the `auto`-turn decision
per session.

This reverses an earlier position, on re-probed evidence (2026-08-21, three
foreign models through `/api/v1/messages`): `stealth/ox-alpha` (reasoning
mandatory — the case that forced the change, an explicit `disabled` 400ing
every request), `deepseek/deepseek-v4-flash` (default-off; thinks when the
block is sent), and `thinkingmachines/inkling` all accept the adaptive block.
inkling is the load-bearing re-test: an earlier probe had it rejecting the
Anthropic block while advertising reasoning support, which is why metadata was
previously kept out of the decision as an invalid signal. That rejection no
longer reproduces — retired as fixed upstream (OpenRouter's translation,
presumably), not as wrong at the time.

---

### Which upstream serves the model

`Provider` above is the *dialect* choice — Anthropic or OpenRouter, where the
POST goes and how it authenticates. A separate question sits one level below it
on OpenRouter: which **upstream** actually serves a given model slug. One slug
(`google/gemini-3.7-flash`) is fronted by several endpoints (`google-ai-studio`,
`google-vertex/global`, each with flex and priority variants), and OpenRouter
picks among them by price unless told otherwise.

`--upstream <slug>` (repeatable) sets `api::Upstream`, which rides the agent
client and serializes as OpenRouter's `provider` object — an order plus
`allow_fallbacks: false`, because a pin that silently reroutes is not a pin. The
type is not called `Provider` because that name is taken by the dialect enum;
the wire rename is the only place the two meanings meet.

Three properties worth knowing:

- **Unpinned is byte-identical to before the field existed.** `None` skips
  serialization entirely rather than emitting `null`, so adding this did not move
  any session's cached prefix. Pinned, it is a per-session constant like `model`,
  so it never toggles in flight.
- **It does not reach the spotter.** Endpoint slugs are per-model and the spotter
  runs a different (Haiku-band) model; an order naming endpoints that do not
  serve it would fail outright with fallbacks disabled.
- **Endpoint choice is measurable, not cosmetic.** The endpoints behind one slug
  differ in uptime and latency tail. On 2026-08-28 an eval run against
  `google/gemini-3.7-flash` took twelve consecutive mid-stream aborts — the whole
  retry budget, ~3.5 minutes — on a Vertex endpoint whose p99 latency sat near
  the client's timeout, while an equally-priced Google AI Studio endpoint was at
  99.95% uptime. Controlled runs should pin and record it
  (eval/harness/README.md).

## Token-level deltas — the `output` class

Assistant prose and tool stdout are the same thing: ephemeral, pre-commit
output that the committed log entry supersedes once the turn closes. One
mechanism carries both.

`Shared` holds a **lossy** `broadcast::Sender<StreamDelta>`. `StreamDelta`'s
variants: `AssistantText` and `ThinkingText` (fragments from `text_delta` /
`thinking_delta` events), `ToolCallStart` and `ToolCallInput` (a tool call
opening, then its arguments arriving), `ToolOutput { call_id, chunk }` (a chunk
from a running tool's stdout or stderr), and `BgOutput { handle, chunk }` (the
same for a background process).

Each control-stream connection with `?include=output` subscribes and receives
deltas as `output` frames as they arrive ([control-api](control-api.md)).

**Deliberately lossy.** A lagging consumer drops deltas rather than stalling
the producing loop. The log is the authoritative backstop; ephemeral deltas are
only a live view. Where the `log` class is durable (with resume cursors),
`output` is best-effort.

**ID-less by design.** Output frames carry no SSE `id:` field. That absence is
the contract that keeps them off the durable resume cursor. `StreamDelta` is
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

**`attach` renders both classes from one connection.** Live fragments
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

### Whether it runs at all

The keepalive is armed off the boot capability lookup and one config knob.

`ModelCapabilities::supports_prompt_caching` answers "is there a prompt cache
behind this model at all". Anthropic is always true (explicit `cache_control`).
On OpenRouter it is read off the endpoint listing, from either of two sufficient
signals: an endpoint that reports `supports_implicit_caching`, or one whose
`pricing` carries an `input_cache_read` — which is how the
explicit-`cache_control` upstreams report having a cache (every Anthropic
endpoint on OpenRouter reports `supports_implicit_caching: false`, because their
caching is explicit; pricing is what distinguishes them from an uncached one).
**Any** rather than all: sticky routing pins a session to one upstream, so the
question is whether a reachable endpoint caches. False, and the loop never
spawns — a `max_tokens: 0` ping against an uncached model refreshes nothing and
re-bills the whole prefix at the input rate every interval, which is the one way
this mechanism can cost more than it saves.

`[transport] keepalive-interval` (seconds, `config::TransportLimits`) is how long
the cached prefix may go untouched before the ping. The default is **240** — a
margin under Anthropic's five-minute ephemeral TTL — and it is a default rather
than a constant because the TTL is a provider fact the harness reads nowhere: a
provider with a shorter window needs a shorter interval to stay warm, and one
with a longer window is paying for pings it does not need. **Zero turns the
keepalive off**, which is the honest spelling for that intent — a separate
boolean could disagree with the interval sitting beside it. The poll period
follows the interval (a quarter of it, capped at 20s) so the ping lands near the
interval rather than up to a full check-period late.

The runtime tripwire below stays regardless: the gate is what the metadata says,
and the tripwire is what the endpoint actually does.

### max_tokens: 0 as the keepalive primitive

A `max_tokens: 0` request reads (and thus refreshes) the cached prefix.
`cache_read_input_tokens > 0`, `output_tokens: 0`, instant. The empty assistant
turn is discarded; the log is never touched.

This call is non-streaming by design: `max_tokens: 0` is rejected by the API
when paired with `stream: true`.

That contract is not universal, so the loop carries a **read-only tripwire**:
a keepalive whose usage reports `output_tokens > 0` ends the keepalive for the
session (`daemon/keepalive.rs`). Observed live (2026-08-21): a
mandatory-reasoning endpoint reasons on a `max_tokens: 0` ping — ~4k thinking
tokens per ping, billed as output — turning the free refresh into a recurring
charge. A runtime observation rather than a provider gate, so it also catches
any future endpoint that stops honoring the contract; the offending ping's
usage is still recorded.

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

The loop pings when the touch clock exceeds the configured interval
(`[transport] keepalive-interval`, above) and stops once the real-turn clock exceeds `CAP` (30
minutes of genuine idle). Past that threshold, one cold cache read on resume
is cheaper than indefinite pinging. During active work, real turns keep the
touch clock reset and the keepalive never fires.
