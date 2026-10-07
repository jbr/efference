# Transport & streaming

This layer bridges `project(log)` and the upstream endpoint in whichever wire dialect
the provider speaks — Anthropic Messages (Anthropic directly, or OpenRouter's
`/api/v1/messages` skin) or OpenAI Responses (the Codex backend or the platform API); see
[the Responses dialect](#the-responses-dialect) and [providers](#providers). It turns a
projected conversation into an HTTP request, streams the response back, and hands a
finished `Completion` to the loop — which appends it to the log and forks. Everything described here is transport-only. None of it appends to the
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

**Reassembly.** `finish()` emits the accumulated blocks, in stream order, as
`Payload::Assistant { output: AssistantOutput::Messages(blocks) }`
([log-and-projection.md](log-and-projection.md)). Tool input is the non-trivial part: the
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
projects. A **finalized** stream is marked the same way in one further case: when
its accumulated `input_json_delta` fragments do not parse as JSON. Such a call
cannot be dispatched however the stream ended (a foreign model sends
`message_stop` after fragments cut mid-string), so it becomes the same
record-only partial, keeping the raw fragment as a string, and the caller's
abort-retry budget decides what happens next. Returning a parse error instead
spent none of that budget and put a serde message where the model could read it.
The dividing line from the max-tokens case is deliberate: a
cap-terminated response is *finalized* — it carries a `stop_reason` and usage
and the loop rightly commits it — where an unfinalized stream carries neither.
Finalized → commit; unfinalized → record-but-retry. One exception rides the
stop reason alone: a `tool_use` block in a `max_tokens` completion was cut
mid-arguments (a model stops at `tool_use` after a call it finished, never at
the cap), and whether the fragments parse is the upstream's doing: a cap-cut
`write` arrives as a valid object with its `contents` split across nonsense keys
and no `path`, and is dispatched. So a finalized
completion carrying both `stop_reason: max_tokens` and a call is marked the
same record-only partial, whatever its input parsed to. A failure observed *after*
finalization (draining the already-closed stream) does not unmake the
completion. The marking is what tells an aborted stream from the agent choosing
silence: without it the aborted stream commits as a clean turn — empty or
thinking-only, zero usage — and the two are indistinguishable.

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
`signature_delta` deltas) into `MessagesOutputBlock::Thinking { text,
signature }` / `RedactedThinking { data }` at their stream position in the
assistant turn; `project` renders the blocks in that same order, signatures
preserved verbatim because the API rejects a modified block on replay. Same-model replay only ever happens (the model is locked per session),
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
Claude, omitted foreign); a separate-model peek sends thinking off the same way
unless `--triage-thinking` or `--triage-effort` — for the per-result peeks only; the
idle triage rides the agent's client and thinks as the agent does
([redaction-fork.md](redaction-fork.md)) (`Client::without_thinking`: explicit
`disabled` for a foreign model whose metadata says reasoning is optional, the field
omitted plus OpenRouter's `reasoning: {enabled: false}` for one whose reasoning is
mandatory *or whose source declares nothing* — `ModelCapabilities::reasoning_mandatory`
is three-state, `Some(false)`/`Some(true)`/`None`, from the model entry's
`reasoning.mandatory` where a source has one. `None` shares the mandatory spelling
because the explicit `disabled` is an assertion and a mandatory endpoint answers it
with a 400, so silence is the only spelling that is safe without knowing;
[redaction-fork.md](redaction-fork.md)); the keepalive follows the same family split
independently (`max_tokens: 0` rejects thinking-on). Toggling thinking
keeps the tools+system cache tier valid (see
[log-and-projection.md](log-and-projection.md)), and it's set once per
call-type and never flipped turn-to-turn, so it stays out of the cached
prefix's way. The wire encoding is pinned by
`project::tests::thinking_turn_matches_expected_wire_bytes`. Thinking blocks are
never elided today — that's a deferred generalization of the redaction model
(the log holds them all verbatim).

### Effort: the level axis

`output_config: { effort }` is the second reasoning knob, orthogonal to
`thinking`: `thinking` is the **type** (whether and when the model thinks),
`effort` is the **level**. It is set at boot from `--effort` or
`[transport] effort` (the flag wins), and there is no runtime mutation path:
changing a thinking parameter mid-session invalidates the message-tier cache on
Anthropic, and a replayed thinking block's signature under a changed config is
its own unanswered question. Unset, the field is omitted entirely and the
provider's own default stands — which is why every session that sets nothing
sends the bytes it always did (`request_envelope_matches_expected_wire_bytes`
is unmoved; `with_effort_envelope_matches_expected_wire_bytes` pins the other
side, with `output_config` sitting between `thinking` and `temperature`).

**The level is a string end to end.** The flag, the config key, and the wire
all carry the configured spelling verbatim — there is no local enum of levels.
The vocabulary is the model's own metadata: OpenRouter's model entry
(`GET /api/v1/model/{author}/{slug}`, singular) carries a `reasoning` block
with `supported_efforts` and `default_effort`, fetched alongside the endpoints
listing at boot and parsed into `ModelCapabilities::supported_efforts` /
`default_effort` (an absent block, absent field, or empty list all normalize to
no declared levels). A level the metadata invents is validated against that
list and sent as spelled, not rejected by a local list that cannot know it.

**Boot validation, per source** (`ModelCapabilities::validates_efforts` →
`ModelCapabilities::check_effort`, called from boot). A level is checked only
against a list the source names and whose values are the request's own. OpenRouter
refuses a level outside a list its metadata names, because it maps an unsupported
level to the nearest supported one **silently** — a session could otherwise believe
it set a knob that did nothing (a model whose `default_effort` is already `max`
makes `--effort max` a no-op by coincidence). A model whose entry names no levels is not refused: 137 of 459
OpenRouter models name none, `openai/o4-mini` among them, and o4-mini honors effort
all the same (2,752 reasoning tokens at `low`, the 6,000-token cap at `high`,
2026-09-25). Anthropic's metadata names no levels, and a Codex catalog entry names
Codex CLI's modes rather than the request's values (above), so neither is checked,
and a misspelled level is the provider's own 400 on the first turn.

The boot line prints `effort: {level}` for a checked level, `effort: {level} ·
unchecked, the model's metadata does not bound it` for one sent without a check —
the case where a provider may drop it silently, which nothing else would show — and
`effort: unset · provider default {default}` when the metadata names a default,
which answers what an unset knob will do without a probe.

**The one coupling between the axes**: the levels in `EFFORTS_REQUIRING_THINKING`
(`xhigh`, `max`) require thinking enabled, and the pairing is a 400 from
Anthropic. `EFFERENCE_THINKING=off` is what can take thinking away for a whole
session, so that combination is refused at boot (`api::check_effort_thinking`,
called from boot) rather than forwarded as a provider error on the first
turn. A level outside those two spellings has no known coupling and passes
through.

**Which envelopes carry it.** The effort rides the agent `Client`, so it reaches
`Client::complete` only — the main turn and the fork peek, which is the shared
envelope ([redaction-fork.md](redaction-fork.md)): if one carried it and the
other didn't, the peek would stop reading the agent lineage's cache. The spotter
forces its tool, disables thinking, and runs on its own client, which is never
given an effort. The keepalive omits it by construction — `max_tokens: 0` rejects
thinking-on, so the ping sends thinking disabled/omitted, and `xhigh`/`max` with
thinking off is the 400 above.

**Watch-item (keepalive).** That last omission means a session with an effort set
sends a keepalive envelope that differs from its turns' by exactly this key.
Whether that splits the cache key on Anthropic — so that the ping refreshes a
different entry than the turns read — is unverified. The symptom is readable in
`usage.jsonl`: a keepalive's `cache_read_input_tokens` dropping to only the
tools+system tier while the turns keep reading the full prefix. Not solved here;
watched.

**Foreign slugs, unverified.** OpenRouter's docs show `output_config.effort`
honored on the Messages skin for Anthropic models. Whether the skin *translates*
it to OpenRouter's normalized `reasoning.effort` for a foreign slug is
**unverified** — no live probe has been run. First use on a foreign model should
watch behavior and `usage.jsonl` (a level that changes nothing about output token
counts is the null result). OpenRouter's own `reasoning.effort` maps Gemini 3 to
Google's `thinkingLevel` (with `xhigh` mapped down to `high`) and other models to
the nearest supported level; token consumption per level is provider-internal.

---

## HTTP/2 over trillium-rustls

`Client` uses `trillium-client` with `trillium-rustls` for TLS. ALPN
negotiates HTTP/2 — Anthropic is fronted by Cloudflare, which does not
advertise `alt-svc: h3`, so HTTP/3 is not a live option. HTTP/2 is the path.

## One client per process (`src/http.rs`)

Every outbound request that speaks TCP draws the same `trillium_client::Client`:
the agent's turns and keepalive, the provider metadata and generation-cost GETs,
MCP over HTTP, the OAuth dance, web push, and the dev-proxy. The two Unix-socket
callers (the control client, the host's per-session proxy) build their own — a
connector is bound at construction, and a UDS connector dials one socket path.

`Client::new` allocates a fresh connection pool and spawns a background reaper;
only **clones** share a pool, which is `Arc`-backed. So a per-subsystem client
meant a separate TCP+TLS+ALPN handshake to an origin another subsystem already
had open. Against OpenRouter that was the visible case: the turn stream and the
cost lookups are the same host, and a cost pass runs thousands of GETs, which now
multiplex onto the HTTP/2 connection the stream already holds.

Two properties make one client sufficient. TLS is per-destination — `RustlsConfig`
handshakes only for a secure destination and delegates to plain TCP otherwise, so
`http://` callers share it. And handlers are per-clone: `with_handler` *replaces*
rather than stacks, so the shared client installs none and each caller adds its
own (retry and upstream-status in `api/client.rs`, redirect-following in
`oauth.rs`). The clone keeps the pool; only the handler and the copy-on-write
default headers differ.

A pooled connection belongs to the reactor it was opened on. A process-wide client
is safe across per-test tokio runtimes only because the pool is keyed by origin and
each test binds a fresh ephemeral port; a test reusing a fixed port across runtimes
could be handed a connection whose reactor is gone.

## What every request carries, and what only some do

The `User-Agent` is a default header on the shared client — `efference/<version>
trillium-client/<version>`, this harness's product token first — so every host
sees what software is calling it. Nothing else is a default.

Everything that identifies *who* is asking is set per request by a caller that
knows the destination: the credential, the sticky-routing key, and the
attribution headers below. A default header would ride every clone to every host.

**OpenRouter app attribution** (`Provider::attribution`). OpenRouter attributes
usage by app rather than by `User-Agent`, keyed on `HTTP-Referer`; the other two
headers only decorate an app the referer already identified.

| header | value |
| --- | --- |
| `HTTP-Referer` | `https://efference.net` |
| `X-OpenRouter-Title` | `efference` |
| `X-OpenRouter-Categories` | `cli-agent,programming-app` |

Provider-scoped rather than global because `Referer` already means something else:
RFC 9110 defines it as the page that linked to the request, and hosts act on it —
origin checks, referer-gated access, analytics. Sending it to an MCP server or to
Anthropic would assert a referring page that does not exist. They ride the two
inference POSTs (`complete` and `keepalive`) and not the metadata GETs, which are
not billed usage. Being headers rather than body, they sit outside the cached
prefix.

The app is created public on the first attributed request. OpenRouter's
`X-OpenRouter-App-Visibility: hidden` applies only at creation and is ignored
afterward, so that choice was one-way and was made deliberately: attribution here
is a public record of a harness that already announces itself in its `User-Agent`.

## Transient upstream failures

`Client` installs `trillium-client-retry`'s `RetryHandler` around every upstream
request. The handler retries the replay-safe Messages POSTs (their serialized
request body is an owned `String`) on 429, 500, 502, 503, and 504. Other 4xx
responses remain terminal, so malformed or oversized requests fail without
replaying them. It makes at most four total attempts and has **no** wall-clock
budget (`with_max_elapsed(None)`); an HTTP `Retry-After` is waited out in place up
to 30 seconds, and a longer one surfaces as the failed attempt for the loop's
schedule (below). There was a 30-second budget, and the handler enforces a
budget by clamping each retry's timeout to what remains of it — so after a 503,
the retry of a large uncached prompt on a model whose time to first byte is near
30 seconds was cut off at the clamp, and the attempt failed for the clamp rather
than for the upstream (an overloaded 503 arriving each time, followed by "Conn took longer than 28.4s"). The set is shared by both dialects and is a superset of what OpenAI's
error guide names as retryable (429 rate limits and `slow_down`, 500, 503
`server_is_overloaded`, each with `Retry-After` when present); the guide's
non-retryable 429s — exhausted credit, spend and usage limits — are the body
veto in `api/failure.rs` (below), and Codex CLI retries all 5xx the same way.
Both Responses profiles ride the same handler: the bearer header, the account
header, the `originator` attribution, and the absence of `anthropic-version` are
pinned over a mock upstream in `src/api/test/codex.rs`, as are a 400 open returning
its body once, a 503 open retried and queued, and the ceiling cutting a
Responses stream mid-text.

The retry handler covers failures while opening a response, before an SSE stream
has yielded any deltas. When it gives up, `Client::complete` returns an
`OpenError` carrying its class (`api/failure.rs`): no response at all (a refused
or timed-out connection) and a retryable status are **transient**, unless the
body names a terminal condition; any other status is a rejection of the request
itself and is **terminal**. A transient one joins the loop's schedule below,
exactly as an aborted stream does. A connection that has returned 200 and then dies
mid-stream is not resumed — it may already have published live deltas — but it
is not lost either: the partial assembles as a record-only completion
(finalized-or-marked, above), and the **agent loop retries the whole request**.
The loop makes up to `STREAM_ABORT_ATTEMPTS` (12) attempts per model call with a
doubling pause capped at 30s — about three and a half minutes of waiting in all;
each aborted attempt's partial is appended to the log first, and its usage
(all-zero for an unfinalized stream; the real billed counts for one that
finalized unparseable) is recorded to `usage.jsonl` under its own
`aborted_attempt` origin (docs/log-and-projection.md) rather than `turn` — it
is never evidence about the cache prefix, and tagging it separately is what
keeps `daemon::metrics` and `efference dev cache` from reading it as a main-loop
request. The budget is sized
to **wait the failure out**, because a mid-stream abort is an ephemeral upstream
condition and the alternative on exhaustion is worse than waiting: idling on a
stall needs an operator to continue it, and an autonomous run has none. The cap
is what lets the count be large without the tail becoming a hang — uncapped
doubling over twelve attempts would sleep for over an hour. Because a marked entry projects to nothing, every retry re-sends
byte-identical messages and rides the still-warm cached prefix. On exhaustion
the loop returns to idle, leaving the failure in two places: the record-only log
entries (durable) and the `stall` state topic (below), which carries the attempt
count and the upstream's own words.

**Continue is "retry now".** `POST /loop/continue` — the TUI's `⏎` and the web
client's send, each on an empty composer — retries whatever is waiting on the
upstream, as often as the operator repeats it. After a stall it re-enters the
trajectory with nothing appended, which re-sends the failed request. While a
backoff pause is running (or a triage gate is held, below) it ends the wait, so the
next attempt fires at once; the daemon accepts it then even though the loop is
running. The signal lives on the retry cell (`UpstreamRetryCell::hurry`), beside
the retry state both loops already publish, because the fork peek's wait runs
where only the client is in reach. Both clients add the gesture to what they show —
`↻ … · ⏎ retries now` during a pause, `⚠ … · ⏎ to retry` on a stall — so the
daemon's own `message` names what happened and not which key recovers it.

**Who is told what.** The model is never told about an upstream failure. If the
harness is in a position to describe the failure to it, the harness has already
recovered from it — and the description is not neutral: a serde parse error
delivered to the inbox is something the model then reasons about mid-task. So the audiences are the **log** (a record-only entry per
failed attempt, for audit), **stderr** (live operator text), and the **operator's
state topics** (`upstream_retry` while a retry is in flight, `stall` once a
budget is spent). Nothing on these paths posts to the inbox.

That idle is **legible as a stall**, not only as an idle. The daemon stays up on
this path deliberately — the socket is alive and one turn recovers the session —
but the return to idle otherwise looks exactly like a finished run to a client:
`running: false` and no gate. So the loop sets the **`stall` state topic**
(control-api.md): the reason, the attempts spent, the instant it gave up, a lead
line naming what happened, and the upstream's words as `detail`. It is
cleared when the work resumes — the top of the next trajectory, or a held gate
retrying — rather than on a successful completion, so it reports the condition
now. A stall is a decision point for `?wait=settled`. The topic is addressed to whatever
is supervising the daemon from outside, which is the rung that can actually send
the recovering turn — an inbox copy would be addressed to the model, which is
neither told about upstream failures nor able to act on one. Without the topic, an earlier inbox notice is the only signal, and a triage peek
can dismiss it — leaving a clean idle. Both clients render the
topic where turn state already shows — the
TUI's status bar, the web composer's turn-state readout.

**Not every in-stream failure is worth the schedule.** Both dialects can open a
stream with a 200 and then deliver the failure inside it — Messages as an `error`
event, Responses as an `error` event or a `response.failed` — and the provider
names it with a type and a code. `api/failure.rs` reads that name: quota and plan
exhaustion (`insufficient_quota`, `usage_limit_reached`, `billing_error`),
rejected requests (`invalid_request_error`, `context_length_exceeded`),
credentials and permissions, and policy refusals are **terminal**; overload, rate
limits, server errors, and every name the table has not met are **transient**.
The class rides the `Completion` beside `upstream_failure`. A terminal failure
takes the record-only append like any abort and then stalls at once with
`StallReason::UpstreamRefused`, carrying the provider's words — the platform quota
error that once ran all twelve attempts is answered once. The same table gates
the transport's own HTTP retry: a retryable status whose body names a terminal
condition (the Codex backend's 429 with `usage_limit_reached` and a reset time)
is not retried, since the status handler stashes the body before the retry
handler decides. An unknown name defaults to transient deliberately, so a code
this table has not met keeps the bounded retry rather than stalling a session on
it; the name stays visible in the record-only entry and the stall detail.

The **fork peek** takes the same treatment on the same budget: an aborted peek
stream or a transiently failed peek request is retried up to
`STREAM_ABORT_ATTEMPTS` times on the same capped schedule, re-sending
byte-identical messages over the warm prefix (its tail is throwaway, so nothing
has to be committed to arrange the replay). On exhaustion — or a terminal
failure, above — the gate does not give the agent anything: it **holds** the
batch's results, unappended, and stalls with `StallReason::TriageUnavailable`
while the loop keeps running, until a continue retries it or an interrupt ends
the turn ([redaction-fork.md](redaction-fork.md)). Withholding the
results as a hard error instead is expensive in a way the turn path is not: a withheld result asks the agent to re-issue a call whose side effects
already stand. In a controlled run a withheld conformance `PASS` was re-earned by
re-running the suite against a still-live server whose counters the invisible
first run had moved, turning the pass into a spurious failure; in another the
agent read the error as its own tool failing and said so. The asides that ride the
same peek machinery — the idle triage and the title — keep a short budget (three
attempts, one fork) instead, because each has a harmless fallback and the loop is
blocked on it meanwhile.

### The duration ceiling

Everything above bounds *failures*. A stream that stays open and keeps trickling
— or stops trickling without closing — is bounded by none of it: the retry
handler covers only the attempt to *open* a response, and a
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

The operator does not have to wait for it: `POST /loop/interrupt` (the TUI's
second `Esc`) ends a read in flight the same way, marked `Aborted` rather than
retryable, and the loop returns to idle ([control-channel.md](control-channel.md)).
An unattended hang still waits on the ceiling. Two shorter clocks are possible
and bound different hangs (backlog/hung-stream-abort.md): a transport-level idle
timeout catches a dead socket, but OpenRouter's `: OPENROUTER PROCESSING`
comment keepalives reset it — the decoder consumes them without yielding an
event — so a live edge in front of a dead upstream is bounded only by a clock on
*events*, which is safe on Messages (thinking streams as deltas) and not on
Responses (Codex may send nothing while reasoning). Neither is implemented.

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

This is never observed directly; the empty-finalized-turn cases are *consistent
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

### What an empty stream leaves behind

The generation ids these carry 404 permanently at OpenRouter, on both
`GET /generation` and `GET /generation/content` (the latter is what would carry
`previous_errors[]` with each upstream attempt's provider and raw body). Nothing
is retrievable after the fact, so the log is the only place an answer can come
from, and this one path keeps evidence with the mark. The mark itself is
`UpstreamFailureRecord` (`src/log/upstream.rs`): a `why` and, on this path only,
`StreamDiagnostics` —

- **status and response headers**, restricted to a named allowlist
  (`CAPTURED_HEADERS`): `x-generation-id` above all, the id every after-the-fact
  query is keyed by, plus `content-type`, `date`, `server`, `cf-ray`,
  request-id spellings, and OpenRouter's cache-status. A response header can
  carry account-scoped values, so what may reach the log is a named set rather
  than whatever the edge attached, and the values still pass the `[env]` masker
  at append like every other stored string ([env.md](env.md)).
- **the `message_start` body as received**, kept whole rather than reduced to
  the fields the assembler models. OpenRouter's Messages skin carries a
  top-level `provider` in the `message` object naming the serving leg
  (verified live 2026-08-28: `"provider":"Google"`), so the captured body
  identifies the leg directly; keeping it whole means any other unmodeled
  field survives too.
- **the frames**, re-rendered from the decoded events in arrival order. Not the
  socket's bytes — the SSE decoder hands over decoded events, so comments and
  line folding are already gone by then.

Frames buffer for every stream (the emptiness verdict is only available at
`finish`) up to `FRAME_CAPTURE_CAP`, 8 KiB, past which the head is kept and
`frames_truncated` says so. An empty stream is a few hundred bytes by definition,
so the cap guards the invariant rather than bounding this path's capture.

The capture reads the executed conn in `Client::complete`, not
`UpstreamStatusHandler::after_response`: an empty stream is a 200, the handler
takes its success early-return, and at that point nothing yet knows the stream
will arrive empty. Delivery does use the existing flow — the marked completion is
appended by the retry loop as the record-only entry it already was.

A record with no diagnostics serializes as a bare string, which is what every log
before this wrote, and either spelling deserializes — so old entries still parse
and the ordinary failure paths write unchanged bytes. None of this reaches the
wire: an entry with an `upstream_failure` projects to nothing
([log-and-projection.md](log-and-projection.md)), so the shape cannot move the
cached prefix.

An exhausted *open* failure (the retry handler gave up) is recorded before it is
judged: the loop appends an empty record-only assistant entry carrying the error
body — Anthropic's 4xx bodies name exactly what was malformed, and the log is the
durable file where stderr is not. A transient one then takes the schedule like an
aborted stream. A terminal one fails the current trajectory rather than the
daemon: it sets `stall` with `reason: "request_failed"` carrying that body as
`detail`, and returns to idle. The model is told nothing either way, and the
projection is unchanged. Keepalive failures are transport-only and leave the log
unchanged.

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

While either loop is waiting out a failed attempt, that wait is its own
level-triggered state topic, `upstream_retry` on `GET /state`
(docs/control-api.md). It names the **surface** (`turn` or `peek`), the attempt
that just failed, the budget it is spending, the absolute instant the next
attempt fires, a lead line — *attempt 2 of 12 failed upstream, retrying* — and the
upstream's own error text as `detail`. Both retry loops publish it (`agent.rs`
for a turn, `distill/peek.rs` for a fork peek, on the agent's cell even when
triage runs on its own model) through one shared constructor, so the surfaces
differ only in the name they carry; the distinction is kept because a triage wait
sits between tool results, where an operator would otherwise read it as the model
generating.
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
429 that recovers on the next try leaves nothing behind but a status and a count,
so the chip cannot be inspected for what actually went wrong. The
handler sits below the loop and cannot append, so the body crosses that boundary
through an in-memory queue beside the failure cell: the observer reads the
non-2xx body once, stashes it on the conn for whoever asks next (the SSE open
error, the keepalive), and queues a copy; the agent loop drains the queue into
log appends after each model call. The queue is bounded and drops its oldest
entries, so an outage against a daemon that is not draining cannot grow the
process without bound — the durable record is the log, not the queue.


### A credential that expires under a running host

`FailureClass::CredentialExpired` is the one rejected-credential case worth
another attempt. It is named by the provider — the Codex backend answers a
ChatGPT access token past its `exp` with a 401 whose `code` is `token_expired` —
and it is checked before the terminal table, because expiry names a terminal
condition too and the only difference is that this one has a remedy. A
credential that is *wrong* (`invalid_api_key`) stays terminal however often it
is re-read.

The value itself is what makes the remedy reachable. An `Endpoint` is built once
per host connection and cloned per session and role, so a credential copied in
as a string would be fixed for the host's life: a token expiring mid-run strands
every session, and even a session created afterwards is born with the dead copy.
`AuthValue` (`api/auth_value.rs`) holds it in one cell that every clone reads
through, so replacing it at the connection repairs the sessions already running.

On that classification `Client::complete` re-reads the credential from its source
and retries the attempt — the same shape as the window backstop above, and once
per call, since a source that yields a working token yields it the first time.
The caller passes the value *it* sent, so several sessions failing on one token
cost one read: whoever arrives second finds the value already past what it sent
and retries on it without re-reading.

Not every kind has a source to re-read, and this is where the auth kinds differ
(`inference.rs`):

| kind | on expiry |
| --- | --- |
| `codex-cache` | re-reads `auth.json`; the operator runs `codex login` in another terminal and the next attempt carries the new token |
| `keychain` | re-reads the store item, which an operator can rewrite from outside the process |
| `oauth` | re-reads the stored grant, and refreshes it against the issuer when its `exp` is near — the only kind whose re-read leaves the machine |
| `env` | nothing to re-read — the value comes from the environment captured when the host started, which no outside change reaches, so a re-read returns the same bytes forever |

An outcome that leaves the same value on the wire (`Unchanged`, `NoSource`, a
failed read) joins the upstream error rather than retrying, so the stall says
what was tried. A replacement is masked before it becomes reachable, in every
session that was protected from the value it replaces (docs/secrets.md); the
maskers are held weakly, since a session's masker dies with the session while the
cell lives as long as the host.

The account id is not re-read: it rides `extra_headers` and is stable across a
refresh of the same login.

**Renewing ahead of expiry.** A source that states when its value expires gives
the cell a renewal deadline, and every request reads the value through
`AuthValue::current`, which re-reads first once the deadline has passed. Only
`oauth` states one today: the access token is a JWT carrying `exp`, and the
deadline is an hour before it. Renewal therefore covers every request path — a
turn, a listing probe (`efference dev models`), a decisions call — where the
rejection-driven re-read above is wired only into the turn's retry loop. A failed
renewal keeps the old value, which may still be accepted until its real `exp`,
logs the source's words, and moves the deadline five minutes out so a failing
issuer is not asked on every request.

A host also renews while idle: at launch it spawns `AuthValue::keep_renewed` for
each connection with a deadline, which renews at once if the stored grant is
already due — so a bad refresh token is reported at launch rather than at the
first turn — and then waits for the next deadline in bounded steps, since a
monotonic timer does not advance while the machine sleeps. The `oauth` re-read
loads the stored grant before refreshing, so a grant that `efference setup` or
another process stored meanwhile is picked up rather than refreshed again. A
refresh the issuer refuses with a 4xx (`oauth::token::Rejected`) is reported as
a login to redo, naming `efference setup`; a transport failure or 5xx is not.

---


## The Responses dialect

A provider speaks one of two wire dialects (`api::WireDialect`), fixed by the
declared provider: Anthropic and OpenRouter speak **Messages** (everything above);
OpenAI speaks **Responses** (`POST /v1/responses`). There is no independent knob —
one session projects and assembles in exactly one dialect, and `Client::complete`
dispatches on it after encoding the shared semantic turns
([log-and-projection.md](log-and-projection.md)).

The recorded log carries its dialect in every assistant entry
(`AssistantOutput::dialect`), and each encoder skips the other dialect's assistant
turns rather than failing. That skip is harmless while a live session is pinned to one
provider and silent loss when a recorded session is rendered or continued under the
other: the model would see a history with its own replies missing. So
`project::check_dialect` runs before either encoder — in `Client::complete` and in
`efference dev wire` — and refuses a turn list whose assistant turns were recorded in a
different dialect, or in both. A session's main provider and model are fixed at
creation and recorded in its boot Preamble; an offline reader checks that recorded
provider against the assistant turns before rendering. A list with no assistant turn
belongs to no dialect and encodes on either. A same-dialect provider or model
selected for a replay is a separate rendering choice, not a live session change.

**Request** (`api/responses.rs`, `ResponsesRequest`; field order pinned by
`responses_envelope_matches_expected_wire_bytes`). `input[]` is the encoded turn
list: the preamble as a leading developer message (a breakpoint-eligible carrier,
which top-level `instructions` is not), user prose as `input_text` parts, each tool
result as a `function_call_output` keyed by `call_id` (bare string, or parts when
images ride along as `data:` URLs), and every logged Responses assistant item
replayed as returned — `reasoning` with its `encrypted_content`, `message` with its
`phase`, `function_call` with its exact `arguments` string (the one exception is a
masked secret, [env.md](env.md): the logged string is then a re-serialization
without it, and that is what replays). `store: false` (the log
is the only history; encrypted reasoning arrives by default in stateless mode),
`truncation: disabled` (a history change is the harness's decision),
`parallel_tool_calls: true` under `auto` and `false` under a forced choice ([README.md](README.md)), `tools[]` as `function` entries
with `strict: false` built from the same `{name, description, input_schema}`
definitions Messages sends, `tool_choice` as `auto` or `{type: function, name}`,
`max_output_tokens` from the same reservation as `max_tokens`, `reasoning: {effort,
summary: auto}` when the model reports reasoning, `prompt_cache_key` carrying the
sticky session id, and `prompt_cache_options: {mode: explicit}` plus the encoded
`prompt_cache_breakpoint` marker when the model's cache policy is explicit. No
`anthropic-version` header rides this dialect; auth is `authorization: Bearer`.

**Stream** (`api/responses/assembler.rs`). Events are typed by the `type` field
of each body. Deltas feed the live view with the `output_index` as the block
index: `output_text.delta` → prose, `reasoning_summary_text.delta` /
`reasoning_text.delta` → thinking, `output_item.added` for a `function_call` →
tool start (its `call_id`, the join key), `function_call_arguments.delta` → tool
input. The committed turn is the terminal event's `response.output`
(`response.completed` / `response.incomplete`), which carries every item whole —
an `output_item.added` reasoning item's `encrypted_content` may be partial, the
terminal copy is complete. `output_item.done` items are held in case a terminal
event never arrives. Every other event type is ignored under one rule: no
semantic output depends on an intermediate event, because the terminal `output`
carries all of it. `error` ends assembly; a stream with no terminal event is a
record-only partial, exactly as a Messages stream with no `message_stop`.

Items are lifted into the log's `ResponsesOutputItem` vocabulary
(`api/responses/wire.rs`): `reasoning`, `message` (assistant role only),
`function_call`; anything else is kept whole as `Unsupported`, and a turn holding
one is record-only — never dispatched, never replayed. Unparseable
`function_call.arguments` are kept as the string and make the turn record-only
too. The stop reason is normalized to the Messages spellings the loop reads:
`completed` → `tool_use` / `end_turn`, `incomplete` at `max_output_tokens` →
`max_tokens` (a call cut there stays undispatched), other `incomplete` reasons
pass through.

**Usage.** Responses `input_tokens` is the whole prompt, cached and written tokens
included; the harness's `Usage` splits input three ways so every consumer's sum is
the prompt total. The assembler restates it: `input_tokens = input − cached −
cache_write`, `cache_read_input_tokens = cached_tokens`,
`cache_creation_input_tokens = cache_write_tokens`. `Usage::prompt_tokens` is the
one sum consumers read for the whole prompt — the reservation anchor and the
`usage.context.used` view both go through it. Responses' separate
`output_tokens_details.reasoning_tokens` is not carried: it is already inside
`output_tokens`, and no consumer states a meaning for the split that would not
be misread as visible thinking.

**Two request profiles** (`api::ResponsesProfile`). The platform endpoint
(`openai`) takes the full documented parameter set above. The Codex backend
(`openai-codex`, `chatgpt.com/backend-api/codex/responses`) is undocumented and
billed to a ChatGPT/Codex subscription, so that arm sends exactly what Codex CLI
sends: the preamble as top-level `instructions` rather than a developer message,
`include: ["reasoning.encrypted_content"]`, the sticky key on `session-id`, and no
`max_output_tokens`, `truncation`, `temperature`, or `prompt_cache_options`, and no
`prompt_cache_breakpoint` markers. On 2026-09-10 the backend rejected each of those
with a 400 `Unsupported parameter`, on gpt-6-astra and gpt-5.6-luna alike, so this is
the backend's surface rather than one model's. `max_output_tokens` has since been
accepted and enforced, with a minimum of 16 (probed 2026-10-02 on gpt-6.1-sol,
gpt-6-luna, gpt-5.6-luna and gpt-5.6-terra); the arm still omits it. The arm's
capability profile reports implicit caching. Explicit mode is unreachable here,
and would be worse than unreachable if half-applied: `mode: explicit` with no
breakpoints disables caching outright. "What Codex CLI sends, and nothing it does
not" cuts both ways — the arm shipped for three days without `session-id`, which
Codex CLI does send, and paid for it in cache misses. Auth is a ChatGPT OAuth access
token sent as `authorization: Bearer`, plus the account it names in
`ChatGPT-Account-ID` and an `originator: efference` header. It comes from one of
two auth kinds:

- `codex-cache` reads the token Codex CLI keeps in `~/.codex/auth.json`
  (`$CODEX_HOME` honoured). efference reads that file and never writes it, so
  renewing is `codex login`'s job — the re-read above is what lets a running host
  pick that up.
- `oauth` is efference's own grant (`inference/chatgpt.rs`): it runs the
  authorization dance itself, stores access, refresh, and id tokens in the secret
  store ([secrets.md](secrets.md)), and refreshes them against the issuer ahead
  of `exp` (renewing ahead of expiry, above). No
  Codex install is required, a Codex that keeps its login in the keyring is no
  obstacle, and renewal is unattended.

The grant is deliberately efference's own rather than Codex's. A refresh may
return a *new* refresh token, so refreshing Codex's grant would leave its
`auth.json` holding a dead one — efference would work and `codex` would break.
Two grants under one client id are the ordinary "logged in from another computer"
case.
Both profiles have a byte pin.

The Codex backend streams with **no `content-type` header** (verified with curl,
2026-09-06). `into_sse` refuses an absent header the way a browser `EventSource`
does, but the failed negotiation still owns the executed conn, so the Responses
dialect recovers the stream from it with `EventStream::new` when the error is
`UnexpectedContentType(None)`; a present non-`text/event-stream` value, and every
other dialect, stay on the open-failure path. Each response may also carry
independently metered `x-<limit>-*` header families. The ordinary allowance is
`x-codex-*`; a separately metered model such as Codex Spark has its own family,
optional `x-<limit>-limit-name`, and may be identified by
`x-codex-active-limit` when exhausted. `api/plan.rs` discovers every family from
its primary-percent header and reads it off every response the status handler
sees — a 429 included — into a cell the daemon publishes as `usage.plan`
(control-api.md). Limit ids normalize hyphens to underscores; each limit carries
its primary and secondary window's used percentage, length, and reset as
absolute millis. A response carrying none of the headers retains the previous
snapshot; one carrying any replaces it whole. The snapshot is operational state,
not a record: it never enters the log, so the captured-header allowlist and the
masker are not involved, and a resumed session starts with `null` until its first
Codex response.

**Turn routing on the Codex backend.** Each response carries an `x-codex-turn-state`
header: an opaque token the backend issues at the start of a user turn and expects
back, as a request header, on every later request of that turn — retries, the fork
peek, each tool round-trip — and not on the next turn. Codex CLI is explicit that
carrying it *across* turns violates the contract and can cause routing bugs, and
holds it in a `OnceLock` so the turn's first value is the one replayed rather than
the most recent (`codex-rs/core/src/client.rs`). It is a per-turn token, not a
running echo — a distinction worth keeping, since the erratic prefix misses this
was first shipped to answer (journal/2026-09-06-codex-session-token-audit.md)
turned out to be the missing `session-id` key instead. The client holds the token in a
cell (`api::TurnRoutingState`) that the response handler fills on the first
successful response of a turn (later values do not replace it), `Client::post`
replays on every request, and the agent loop clears through `Client::begin_turn`
at each trajectory start and each seam-drained user turn. The fork peek shares the
agent's client and so the token; the spotter's client is built
`without_turn_routing`, since each of its requests is its own conversation. Every
Codex request also names its model in `x-codex-routing-hint` (`model=<slug>`), as
Codex CLI does. Both headers are transport-only and never enter the log. The
rejection of `prompt_cache_breakpoint` above is observed on gpt-5.6-luna; OpenAI's
guide lists explicit breakpoints as a GPT-5.6-family feature on the platform, so it
is the backend, not the model, that is presumed to refuse them (backlog:
openai-provider/validation.md).

**The keepalive on this dialect** has no `max_tokens: 0`: its ping is the turn's own
streamed envelope with `max_output_tokens` fixed at 16, the Codex backend's minimum
(`RESPONSES_KEEPALIVE_CAP`), sent on the Codex profile too though a turn there omits the
field. The backend resumes a cached prefix only where an earlier request ended
(backlog: separate-triage-cold-prefix.md), and the keepalive loop's lopped projection is
exactly the last request's input, so the ping reads it whole and refreshes it: a cached
read plus 16 output tokens. A capped request that the upstream refuses is not retried at
another cap. OpenAI exposes no per-generation cost endpoint, so cost tracking stays off.

**The triage primer is the same write on the triage lineage.** Where the peek runs on
its own model (`ForkPeek::own`), the batch fires the turn's own input on the triage
client at the first execution start — post-approval, so nothing speculative is ever
sent — capped at the same backend minimum (`Client::prime`, Responses with an
implicit policy only). The gate's peek then extends a triage-lineage request that
ended where the main one did, and reads everything before the current calls from
cache. A primer the upstream refuses, or a lineage with no implicit cache, resolves
unprimed and the peek goes out as before. Primer spend records under its own
`triage_primer` usage origin, with one log line per gate carrying the send time, the
serial residue the gate path paid, and whether a gate followed. The primer forces
`retain` rather than the turn's `auto`: the backend keys the resume on
`tool_choice`, so the plant must match the peek's head, not the main request it
replays. It tolerates the truncated output: a 16-token budget at a call-eliciting
position cuts the model's function call mid-arguments, which marks the completion
without unmaking its input prefix, and only a response with no accounted input
counts as nothing planted. The
spotter model must be one its connection's metadata source knows: with no
`[defaults].spotter` and no connection reaching jev, the main connection's cheap band applies
(`Provider::default_spotter_model`, gpt-5.6-luna here, claude-haiku-4-5 on
Anthropic and its `anthropic/` slug on OpenRouter), and an explicit `--spotter-model` passes its own provider's
check. Model capabilities on this dialect come from the provider's catalog, not
from the build (**OpenAI capabilities**, under Providers below).

## Providers

The request **body** is the dialect's encoding of the shared semantic projection —
Messages for Anthropic and OpenRouter, Responses for OpenAI — so the log, the
semantic fold, and the cache-frontier model are provider-independent. What varies
between providers is the **dialect**, **where** the POST goes, **how** it
authenticates, and **which metadata source** answers questions about the selected
model. All derive from one declared identity: `api::Provider`, an enum
(`anthropic`, `openrouter`, `openai`, `openai-codex`, `typesafe`, and `custom`)
grown in code as providers are added. Identity is an input, never inferred from URL
shape (sniffing fails for proxies and custom URLs). `api::Endpoint` carries the
derived `url` + `auth_header` + `auth_value` alongside the provider; its `Debug`
redacts the credential and extra headers.

### A provider the operator declares (`Provider::Custom`)

`Provider::Custom(WireDialect)` is an endpoint that speaks a dialect this crate
already has an encoder and an assembler for, at a URL this enum does not know.

A connection either **names** a provider or **describes** one, and the two spellings
are exclusive — exactly one is required. There is deliberately no third spelling
naming the describing case: a `provider = "custom"` line would carry nothing the
`dialect` and `base-url` beside it do not already say, and `dialect` is the
discriminator on its own.

```toml
[connections.meta]
dialect = "messages"                      # or "responses"
base-url = "https://api.meta.ai/v1"
auth = { kind = "env", key = "META_API_KEY" }
```

Both halves are required together, because neither has a default that would be safe
to invent; `dialect` or `base-url` beside a named `provider` is an error, since that
provider derives its own dialect and owns its URLs, so either key could only agree
redundantly or disagree.

The dialect is the variant's payload because it is the one fact no default can
supply. Everything else is either owned data on `Endpoint` — the turn URL, the auth
header, the base (kept beside the URL rather than derived back out of it, since the
metadata and credential-check routes are the turn route's *siblings*, not its
suffixes) — or derived from the dialect: `{base}/messages` or `{base}/responses`
for turns, `{base}/models` for both the capability lookup and the credential check,
and a trailing slash on the base is optional.

**Every per-provider quirk takes its conservative arm, derived from the dialect
rather than inherited from a named provider that speaks the same one.** A sticky
session header, a turn-routing header, attribution headers, `prompt_cache_key`, an
image-placement or breakpoint-placement workaround: each is a fact about one
vendor's deployment, so copying a named provider's set onto an arbitrary endpoint
would send it another vendor's workarounds, and an unrecognized header may be
rejected rather than ignored. So a custom endpoint is sent `authorization: Bearer`
and nothing else beyond the dialect's own headers; `prompt_cache_key` rides only on
Responses, where it is a documented field, because an unrecognized top-level key is
a 400 on Messages. Temperature maps to 0..1, the range both scales accept.

Two consequences of not knowing the catalog. `Provider::default_spotter_model` is
`None`, so resolution asks for `[defaults].spotter` with an issue rather than
guessing a slug that would fail at the metadata lookup. And effort levels are not
validated (`validates_effort_levels` is false): no listing observed declares them,
and `ModelCapabilities::check_effort` refuses every level when none are declared, so
validating here would make `effort` unusable rather than merely unverified.

`GET /state` and the boot line report the provider as `custom-messages` or
`custom-responses` — the bare word would not say what the session speaks.

#### Capabilities from a model listing (`src/api/provider/listing.rs`)

A named provider has a capability source whose shape this crate knows. A custom
endpoint has only what `GET {base}/models` returns, and that ranges from the bare
OpenAI object — `id`, `object`, `created`, `owned_by` — to a listing carrying the
window, the output cap, modalities, cache pricing, and the accepted sampling
parameters. Measured against two real endpoints, one sits at each extreme.

So the parse is **opportunistic and field-by-field**: each capability is read if a
spelling for it is present and left unknown otherwise, and a listing that answers
nothing is a valid answer. The spellings are the ones observed on real endpoints
rather than an invented superset, and where two endpoints spell one capability
differently both are read — `context_length` or `max_input_tokens` for the window;
`max_output_length`, `max_output_tokens`, `max_completion_tokens`, or `max_tokens`
for the cap; `input_modalities` flat or nested under `architecture`;
`input_cache_read(s)`/`input_cache_write(s)` pricing, where a read price means a
cache exists and a **nonzero write price** means the caller chooses what is written.

The parse is **not keyed off the dialect**: a listing's shape and the dialect an
endpoint serves turns in are independent axes. One endpoint serves a bare
OpenAI-shaped listing in front of both dialects; another serves a capability-rich
listing in front of a dialect this crate does not speak at all.

A model the listing does not name reads as `ModelCapabilities::unknown` rather than
an error. A listing can be paginated and can omit an alias the endpoint still
serves, so it is in no case a statement about what the endpoint will accept, and
refusing to boot over one would turn a non-authoritative document into an
authorization check. Every unknown then takes the arm that asserts least: no image
input (sending bytes a model cannot read spends the upload and reports success),
`PromptCachePolicy::None` (which disarms the keepalive), temperature accepted (the
value passes through to the endpoint's own validation), and no reasoning
requirement — which is why that field is three-state rather than a `bool`.

#### An unreported output cap

`max_tokens` is **required** on Messages, so an unreported cap cannot be answered by
omitting the field, and an asymmetry between the dialects would cost more than one
rule. `UNKNOWN_OUTPUT_CAP` (65536, `api/reservation.rs`) is what boot sends instead,
stated on the boot line as an assumption — `max output 65536 assumed (none
reported)` — rather than printed as a fact.

The number is chosen by how many models **accept** it, not by how many declare it.
Across the 327 non-variant models OpenRouter reports a cap for, 61% sit at or above
65536 against 38% for 128000 — the single most common value, but a popular ceiling
rather than a popular floor. It is also the median and the minimum across the models
this project runs, so in practice it is refused by none of them.

Erring low is the worse direction, and only down to a cliff. A cap too low for a
mandatory-reasoning model spends the whole budget on thinking and returns
`content: []` with `stop_reason: max_tokens` — an empty turn, not a short one,
reading as a broken model. Above that cliff the asymmetry flattens: a cap set too
high costs one refused request and gains nothing, since a turn wanting more than
65536 output tokens is not a case this harness has.

That refusal is recoverable. `reservation::output_cap_rejection` classifies the
*other* reservation 400 — the cap itself above what the model accepts, as distinct
from the prompt-plus-reservation overflow `is_window_overflow` matches. The two are
told apart by the window: every overflow phrasing names the *context*, and a cap
rejection names only the output parameter and its ceiling. Where the provider states
that ceiling — after a comparison (`max_tokens: N > M`), as a bound (`a value <= M`),
or as a phrase (`at most M`) — the retry goes straight to it rather than searching;
where it states none, the same halving applies, so a fixable 400 is never terminal.

A stated ceiling is then **remembered for the session** (`Reservation::learned_cap`).
Without that the correction would last exactly one request, since `compute` reads the
client's configured cap fresh each time and the next turn would send the refused
number again. A wrong guess would cost a wasted round trip *per turn* rather than
once, which is what makes guessing affordable at all.

`ModelCapabilities::context_length` is therefore `Option<u64>`. The consumers
already tolerate its absence — `Client` and `api/reservation.rs` take an `Option`,
and `GET /state` reports a null `context.limit` — so what an unknown window costs is
the occupancy meter and the pre-emptive output fit, not the session. The provider's
own refusal remains, and the halving retry answers it. The boot line says
`window not reported` rather than omitting the field, so the assumption is visible
instead of surprising.

### A provider that serves no turns

`typesafe` is the first provider with **no inference dialect**:
`Provider::inference_dialect` returns `None` for it, and `Endpoint::url` — the
turns URL — is `None` too. It serves one route, the decisions route below, which
answers typed questions about a state rather than producing an assistant turn.
There is no dialect it could honestly claim, so it claims none.

The absence is enforced once, at resolution (`src/inference/options.rs`): the
`model` and `triage` roles refuse a provider whose `serves_inference()` is false,
with an issue naming the spotter as what it *can* back. Every reader downstream —
the encoders, the assemblers, `Client::dialect`, `Client::url` — therefore treats
the dialect and the URL as present, because the only way to reach them is through
a role that already refused the alternative.

### The decisions route (`src/api/decisions.rs`)

A **decision model** is an LLM with generation replaced by a classifier head. One
POST of `{model, state, questions}` returns one JSON body: a calibrated
probability per question, no stream, no assistant output, no text.

```json
{ "answers": { "in_scope": { "type": "noul", "noul": 0.91 } }, "usage": { "input_tokens": 1081 } }
```

`noul` is the API's own name for the boolean question type, on both the asking and
the answering side, and is spelled as the wire spells it. Answers are parsed
leniently — a shape this harness does not model yet yields `None` for that
question rather than failing the response away, and the caller's `None` handling
is the same fail-closed path a transport error takes.

Two providers front the same models and the request differs slightly between them:
OpenRouter documents `session_id`/`user`/`trace` alongside the three required
fields, while the first-party API documents the three alone and validates the body
(an unrecognized key is a 422, which for this route means a failed permission
check). `Provider::decisions_takes_session_id` is where that difference lives, so
the grouping key rides only where it is named. Attribution headers follow the same
rule they already do — OpenRouter's, nobody else's.

The route retries 429 and the 5xx family on a short budget — three attempts inside
five seconds, a longer `Retry-After` not waited out. The inference client's budget
is thirty seconds because a turn waiting on a model has nothing better to do; a gate
has a fallback, the human, which an operator at the keyboard answers faster than a
long backoff. The budget exists so a brief rate-limit burst does not turn every
gated call in it into a human gate.

Nothing on this route caches (the model's endpoint metadata reports
`supports_implicit_caching: false` and there are no markers to send), so the
byte-stability rule that governs the Messages prefix
([log-and-projection.md](log-and-projection.md)) has nothing to hold here. The
`RequestUsage` a response reports carries **zero** cache counts — a truthful zero
rather than a missing reading, which is what keeps a decisions request out of the
cold-streak classification.

A decision model also publishes no per-model capabilities: there is no context
window, output cap, or cache policy behind a decisions request. `capabilities_url`
errors for it. Boot instead checks the id against the provider's model listing
(`GET /v1/models`, shaped `{"models": [{"name": …}]}`, aliases like `jev-latest`
listed by name), so a misspelled decision-model id fails session creation naming the
ids that exist, rather than booting clean and failing every gate closed. The listing
GET is authenticated, so it is the credential check too.

**Host-owned connections (`src/inference.rs`).** A host names its connections in
`~/.efference/config.toml`, read from the host's startup `HOME` — a secret-free
file `efference setup` writes interactively ([operating.md](operating.md)): each `[connections.<name>]` declares a `provider` and where its credential
comes from (`auth = { kind = "env", key = "VAR" }` reads the named variable from
the host's startup environment, ordinarily supplied by `op run`; `auth = { kind =
"keychain" }` reads the secret store's `inference:<name>` item, `name` defaulting
to the connection's ([secrets.md](secrets.md)); `auth = { kind = "codex-cache" }`
reads Codex CLI's `auth.json` once, under the host's `CODEX_HOME` or `HOME`). Credentials load once at host launch, each connection independently:
an unavailable one carries its reason and fails only the sessions that select it.
Nothing re-reads a caller's environment, re-runs a secret command, or re-opens the
cache per session, and the host's environment is never overlaid onto a session's
snapshot or a tool child.

A session selects a model per role as `<connection>/<native model>`. A built-in
provider name is always a connection prefix, configured or not (`anthropic/…` on a
host without an `anthropic` connection names the missing connection). A bare id
with no connection prefix is refused — with no default model there is nothing
to resolve it against; a native id equal to a connection name must be written
qualified. An omitted `--model` takes past picks: the project mode once the
project holds about five effective explicit selections after decay, else the
system-wide aggregate, else a refusal naming the choice. Only explicit human
picks are remembered — never an inherited default, so the default cannot
reinforce itself — keyed by working root and origin config, with a single
exponential decay (half-life 30 days) over both reads. A remembered connection
that is no longer usable falls through silently, since no request named it. An
omitted spotter
takes `[defaults].spotter`, else the main connection's `spotter-model`, else jev on a
connection that reaches it, else the main connection's cheap band
([permissions.md](permissions.md)); an omitted
triage shares the main client, whatever main resolved to. Defaults are
written fully qualified, so adding a connection cannot reinterpret one. All three
roles resolve before a session directory exists; a main/triage pair on different
dialects is refused there, naming both. Each role gets its own clone of the
connection's endpoint with its own sticky key, so two sessions on one connection
share a credential, not a client. The native id alone is what a request carries;
the connection name appears in boot output, `session_identity`, and the
boot log metadata. The main role's connection, provider, and native model are
written once to the session's boot Preamble as unprojected log metadata in the
log-owned identity shape, so an offline reader does not have to infer the
session's provider from current config or usage telemetry. Picks live in
the `model_picks` table of the user state database
([state-store.md](state-store.md)), beside the config file that named it; the same store
answers the recent ranks in the model catalog (`GET /inference/catalog`,
[model-catalog.md](model-catalog.md)) and the offline `efference dev history`
reader. The provider listings behind the model menus, their normalization, and
their storage in the same database are described in
[model-catalog.md](model-catalog.md).

The dev verbs that dial a provider without a host (`dev peek`, `dev spotter`,
`dev spotter-eval`) load the same file from their own environment and resolve
their `--model` through it. `dev wire` has no model override: it reads the
connection, provider, and native model from the log's boot Preamble, derives
placement from that identity, and uses the recorded dialect and model for both
the full envelope and `--messages`. It never resolves an inference endpoint. A
log without an identity uses its recorded dialect and default placements, with
a note; only a legacy full envelope may load the current role config to rebuild
a missing system snapshot. Nothing reads a provider from environment variables.

**OpenAI capabilities** come from the provider's own model catalog rather than
from a table in this build (`api/provider/codex.rs`), so a model released after
the build resolves the same as one released before it. Both arms read the
subscription backend's catalog, cached per connection for five minutes and shared
by a session's main role, its spotter, and any triage. An entry reports the
context window the backend says the model works in, its reasoning levels and
default, and its input modalities; a model the catalog does not name passes on the
arm's baseline instead of failing boot, and the first turn is the test of whether
the provider serves it.

- **`openai-codex`** reads it as its own source: a catalog that cannot be read,
  with nothing cached, fails the lookup.
- **`openai`** reads it as a borrowed one. The platform Models endpoint (list and
  per-model alike) reports `id`, `object`, `created`, and `owned_by` and none of
  the limits a boot path consumes, while the catalog route answers a platform key.
  So the platform key is also sent to `chatgpt.com`, and a failed read logs a
  warning and boots on the baseline rather than refusing the session. The
  catalog's windows are the subscription backend's, which may be smaller than the
  platform's; the smaller figure costs an early compaction, where a larger one
  would cost a 400.

The catalog reports no output cap, no temperature acceptance, and no cache
policy, and those are read as the arm's baseline asserts: a model that rejects
`--temperature` is refused by the provider rather than by the harness, and an
unreported cap is an unreported cap (the boot line says so). The cache policy
is the one exception, because it is a property of the backend rather than of a
model — the Codex backend rejects every explicit cache parameter on every model
it serves, so that arm reports implicit, and the platform accepts explicit
breakpoints, so that arm reports explicit. Effort levels are the one capability a boot path
*checks*, and only where a source's list is the request's own list: a Codex
catalog entry names what Codex CLI offers, which includes a delegation mode the
Responses API rejects and omits values it accepts, so its levels are printed on
the boot line and sent as configured, and the provider's 400 — which names every
value it accepts — is the check (`ModelCapabilities::validates_efforts`, and
**Boot validation, per source** above).

**The listing route** (`Provider::models_url`, read by `Endpoint::probe`) is a
GET whose status, `ETag`, and body survive to the caller whatever the status,
carrying the identity a turn carries — auth, the provider's extra headers, its
attribution, and `anthropic-version` where the dialect is Messages — because a
catalog scoped to an account answers only to that account's headers, and a
header added to an in-flight request is not part of the request that went out.
`efference dev models {connection}` is its reader; on the Codex arm it is also
the capability source above, and on the others it supplies model-catalog rows
([model-catalog.md](model-catalog.md)).

The two OpenAI arms' catalogs are separate routes. `https://api.openai.com/v1/models`
is the platform listing and answers a ChatGPT grant with 403 naming a missing
`api.model.read` scope: the grant authenticates, and is not a platform key. The
subscription backend serves its own catalog at
`https://chatgpt.com/backend-api/codex/models`, the route Codex CLI reads; it
answers a platform key as well as a grant (200, the same entries), and refuses an
unauthenticated or malformed bearer with 401. That route requires a `client_version` query parameter, and each entry
carries a `minimal_client_version` the backend filters on — so which models a
client is shown follows the version it claims, and a version below every entry's
minimum answers 200 with an empty catalog rather than an error. This build
claims the newest generation rather than its own version, which is the whole
list of models any client can see; the models gated highest are the ones released
most recently.

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

The pin expires after five minutes of inactivity and is refreshed only by a
*successful* request; an errored one does not refresh it (OpenRouter's "Provider
Sticky Routing"). So a keepalive that fails also lets the pin lapse across any idle
over five minutes, after which the next real turn may route to an upstream that
never saw the cache — the ping keeps both the prompt cache and the routing pin
alive, and loses both together.

Which requests carry it is the **provider's** call: `Provider::session_header`
returns the header name, or `None` where the concept doesn't exist. Anthropic
returns `None` — nothing in Messages selects an upstream, because there is only
one, and `metadata.user_id` is an abuse-detection identifier rather than a
routing key. An earlier draft sent the header unconditionally on the grounds that
an unrecognized header is free; gating on the declared provider means that
assumption never has to hold. Offline verbs (the `spotter` probe, `wire`) derive
no key at all — a one-shot probe has no conversation to pin.

The Codex arm reads the key on **`session-id`**, the spelling Codex CLI sends
(`codex-api/src/requests/headers.rs`; the backend answers to `session_id` too).
The backend echoes whatever arrives there back as the response's
`prompt_cache_key`, and with no header it mints a fresh UUID per request —
`prompt_cache_key_changed` is one of the miss reasons OpenAI's own diagnostics
enumerate, so a per-request key means the prefix cache is matched on luck alone.
That arm was grouped with the providers that read no key until 2026-09-10, on the
reading that it took the key in the body; it accepts the body field and discards
it, which is why the gap stayed invisible. Measured on one workload run twice,
minutes apart: turns 30% warm and peeks 40% without the header, 80% and 100% with
it. Codex CLI's companion `thread-id` sets no key on its own and is not sent.

The same value also rides the **request body** as `prompt_cache_key`, on the
providers that read it (`Provider::prompt_cache_key_field` — OpenRouter and the
two OpenAI arms; Anthropic's Messages API has no such field and 400s an
unrecognized top-level key, so this cannot be sent unconditionally the way a
header could). On the Codex arm that body copy is inert — accepted, discarded,
and superseded by the header above — and is kept only so the arm starts
benefiting if the backend ever begins honouring it. It is
deliberately redundant with the header, and third in OpenRouter's own precedence
order — body `session_id`, then `x-session-id`, then `prompt_cache_key`
(verified against their prompt-caching docs, 2026-08-28). It is in the body
because a header is the part of a request an intermediary may drop, and because
an upstream that speaks the OpenAI dialect keys *its own* prompt cache on this
field rather than on OpenRouter's routing key. Like `model` and the upstream pin
it is a per-session constant, so it cannot toggle mid-session; unset, it skips
serialization entirely, so adding it moved no Anthropic session's bytes.

The spotter takes a **distinct** key (`Endpoint::for_spotter`, a `-spot` suffix), and
so does the separate-model fork peek when one is configured (`Endpoint::for_triage`,
`-triage`; [redaction-fork.md](redaction-fork.md)).
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

**The boot capability query.** Boot fetches the selected model's metadata
from the provider before the daemon starts (`Endpoint::fetch_capabilities`,
`api::provider`), and a failed lookup — network, non-2xx, missing fields —
**aborts startup** with the URL and body in the message. The query is required
rather than best-effort because capability conditionals hang off the result
and "unknown" would silently disable them; there is no offline mode to
regress (boot already requires the live messages endpoint). Anthropic asks
one route; OpenRouter asks two, both fatal at boot — the effort-levels route
is what the boot validation validates against, so "the entry route didn't
answer" must not degrade into no validation. Per provider:

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
  change price and availability as a side effect of a display value. The min
  is taken over the endpoints at or above `OUTPUT_FLOOR` (32k, `api/provider.rs`):
  an outlier far below its peers would otherwise cap every turn of the session
  at its number, which on a model whose thinking counts against `max_tokens`
  is a turn cut off before its first tool call (`z-ai/glm-5.3-flash`,
  2026-09-02: twenty-one endpoints at 128k or more, one at 2048; three of the
  session's turns ended at 2048 with thinking only). Excluding the outlier
  from routing is the intended effect. Only when no endpoint clears the floor
  does the plain min stand — a short-output model gains nothing from narrowed
  routing. The
    model-level fields are not used: `context_length` there is the max across
  endpoints, while `top_provider` describes one designated endpoint (not the
  originating lab, and not necessarily the cheapest), and the two disagree.

  The ceilings route is the first of OpenRouter's two boot fetches. The second
  is the singular model entry, `GET /api/v1/model/{author}/{slug}`, whose
  model-level `reasoning` block carries `supported_efforts` and
  `default_effort` — the metadata the boot validation validates against
  (the effort section above). It is a separate route because the endpoints
  listing carries no `reasoning` block at all, and its model-level fields stay
  unused; the entry fetch is equally fatal at boot.

When `--upstream` pins the routing (below), the pin **narrows the endpoint set
before anything aggregates over it**: a pinned session routes to exactly those
endpoints, so a ceiling taken across the ones it will never reach is someone
else's. A pin naming no endpoint in the listing falls back to the whole set
rather than emptying it — that request fails loudly on its own, and an empty set
here would abort boot with a message about `context_length` that names the wrong
problem.

The parsed `ModelCapabilities` (`context_length`, `max_output_tokens`,
`supports_reasoning`, `supports_image_input`, `prompt_cache`,
`supported_efforts`, `default_effort`; grown
one field per consumer) is transport-only — held
in `Session`, never logged or projected. `max_output_tokens` sets the
per-turn output cap (`Client::with_max_output`): a reasoning model's
thinking bills against `max_tokens`, and the pre-reasoning 4096 default —
which stands only when the provider reports no cap — starves a long think,
terminating the turn at the cap before any text or call arrives (the committed
turn is thinking alone). A cap-terminated
turn commits whatever arrived and posts a notification naming the cap
(`agent.rs` on `stop_reason: "max_tokens"`) — before that existed, the
session just sat idle, reading as a hang.

**`max_tokens` is a reservation, not a prediction** (`api/reservation.rs`).
The provider sets the tokens aside before generating and rejects a request
whose prompt plus `max_tokens` exceeds `context_length` — Anthropic as
"input length and `max_tokens` exceed context limit", OpenRouter as "maximum
context length is N tokens. However, you requested about M tokens". So a
model whose advertised output cap is most of its window (`meta/muse-spark-1.3`:
943,718 of 1,048,576) walls the session at a tenth of the window if the cap is
sent verbatim, and a 200k model with a 128k cap walls at 72k. Each request
therefore sends the cap shrunk to what the window spares:
`min(cap, context_length − estimated_prompt − 2048)`, never below 1024. The
prompt is estimated from the last response's exact prompt count (`input` +
cache read + cache write, recorded against the byte size of the body that
produced it) plus the bytes the body has grown or shrunk since, at three bytes
per token — the conservative side of the 3.5–4.4 measured across the corpus's
models, so the estimate errs toward reserving less. Until a response has been
counted, the whole body is estimated at that ratio. Far from the wall the
value sent is the cap; near it, the reservation shrinks each turn. A 400 in
either provider's overflow phrasing is the backstop: the reservation is halved
and the request retried, once per halving down to the floor, and any other
open failure stays terminal. Turns, fork peeks, and triage all share the one
client and so the one calibration; the keepalive's `max_tokens: 0` ping and
the spotter's own client (no window set) are outside it.

`GET /state` surfaces the window as `usage.context = {limit, used}`, where
`limit` is `context_length` itself and `used` is the last request's total
prompt tokens (`input_tokens` + `cache_read_input_tokens` +
`cache_creation_input_tokens` — `input_tokens` alone is only the uncached
remainder; null until the first request). `limit` is deliberately not reduced
by the output reservation: that reservation shrinks as the prompt grows, so
subtracting it would move the reported ceiling through the session and read as
a wall the prompt never reaches. The prompt stops fitting
`MARGIN + RESERVATION_MIN` short of the window (`api/reservation.rs`), so
occupancy approaches 100% rather than reaching it at the stall. The TUI
header draws it as a context meter captioned `14k/200k`, and the session drawer's
Usage section leads with `context  7% · 14k/200k`.

**Cache-read telemetry.** The same `/state` topic carries `usage.cold_streak`:
consecutive main-loop requests that read nothing from the prompt cache and had
no reason not to (not session boot, not a cross-model replay, not idle past the
TTL).
It is the live end of the empirical prefix oracle — the byte pins prove the
harness emits what it emitted yesterday, and only a read rate proves the
provider still matches it. The classifier, the threshold and the corpus it came
from, and the offline `efference dev cache` view are in
[log-and-projection](log-and-projection.md).

**Session cost telemetry.** Where the provider has a per-request accounting
surface, every recorded request (turn, peek, spot, triage, keepalive — plus
keepalive pings funnel through the same two record methods) spawns a
best-effort fetch of its **billed** cost, folded into a running total that
`GET /state` reports as `usage.cost = {usd, unpriced}` and the TUI renders
in the header (`$0.04`) and the session drawer (`cost  $0.0421`). The one exclusion is an `aborted_attempt` line with all-zero
usage: nothing to price, and fetching it would only run the lookup to
exhaustion and mark it `unpriced` — an undercount signal for a total that was
never wrong. An aborted attempt that finalized unparseable billed real tokens
and is priced like any other request. Cost is fetched, never modeled from a rate card —
cache read/write/TTL multipliers and per-endpoint price variance make modeling
brittle. OpenRouter is the one provider with such a surface today:
`GET /api/v1/generation?id=…` returns `total_cost`, keyed by the generation id
each response carries (the same id `usage.jsonl` records — the join to the
provider's own accounting). The record lands some time after the response
completes and a too-early GET 404s; the lag is the upstream's (seconds for
Anthropic-served models, over a minute for `meta/muse-spark-1.3`), so
the fetch backs off across six attempts (2s through 300s, `COST_FETCH_DELAYS`
in `daemon/metrics.rs`); exhaustion increments `unpriced`, so the total is a
legible undercount rather than a silent one. On direct Anthropic there is no
per-request cost surface (the Models API has no pricing; the Usage & Cost
Admin API reports realized org costs in daily buckets under a separate admin
key) — `usage.cost` is null there and the TUI omits it.

**OpenRouter** is reached by declaring a connection on it and a provider-scoped
model slug
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
provider-conditional: the portable one leaves a trailing tool suffix unmarked, and
Gemini slugs on OpenRouter send no marker, because a marker there pins the read
to a Gemini explicit cache that a tool loop never advances)
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
Anthropic block while advertising reasoning support, which is why metadata stays
out of the decision as an invalid signal. That rejection does not reproduce — retired as fixed upstream (OpenRouter's translation,
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

`Session` holds a **lossy** `broadcast::Sender<StreamDelta>`. `StreamDelta`'s
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

**The TUI renders both classes from one connection.** Live fragments
print first; the committed entry finalizes the line when the turn closes. Tool
output is intentionally not deduped against the committed `tool_result`: the
stream shows raw live output, the result shows the fork's distillate (see
[redaction-fork.md](redaction-fork.md)). Both carry signal.

---

## Cache instrument panel and keepalive

Caching shapes this layer (see [log-and-projection.md](log-and-projection.md) for the
TTL-is-a-write-time-property background). It
is invisible and dies at the 5-minute TTL on any idle. This layer
makes it legible and keeps it warm, entirely within transport and daemon — no
log appends, no projection changes, fully reversible.

### Whether it runs at all

The keepalive is armed off the boot capability lookup and one config knob.

`ModelCapabilities::prompt_cache` records who chooses write boundaries:
`explicit`, `implicit`, `mixed`, or `none`. Anthropic is `explicit` through
`cache_control`. On OpenRouter it is read off the endpoint listing: an endpoint
that reports `supports_implicit_caching` contributes implicit support, while one
whose `pricing` carries an `input_cache_read` contributes explicit support —
which is how the explicit-`cache_control` upstreams report having a cache (every
Anthropic endpoint on OpenRouter reports `supports_implicit_caching: false`,
because their caching is explicit; pricing is what distinguishes them from an
uncached one). Both signals across the reachable set produce `mixed`. Sticky
routing selects one upstream but does not report which cache policy it selected,
so the distinction remains explicit in the capability rather than being folded
back into a boolean. `none` turns the keepalive off: a `max_tokens: 0` ping
against an uncached model refreshes nothing and re-bills the whole prefix at the
input rate every interval, which is the one way this mechanism can cost more
than it saves.

`[transport] keepalive-interval` (seconds, `config::TransportLimits`) is how long
the cached prefix may go untouched before the ping. Absent, it is four fifths of the
provider's cache lifetime (`Provider::cache_ttl`, `daemon::keepalive_interval`): 240
seconds against the five minutes Anthropic documents and the conservative reading for
providers that document nothing, 8 minutes on OpenAI's two arms. OpenAI documents 30
minutes since the last write or reuse, but a gpt-5.6-luna ping 16 minutes after its
last request read cold and the corpus has the 5.6 family mixed past 7 minutes
(journal/2026-10-05-cache-lifetime-corpus.md), so those arms assume 10. A late ping pays
the full read; an early one costs a cached read. No provider
exposes the lifetime to query, so a set value is the override. **Zero turns the
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
a keepalive whose discarded output is large relative to the prefix it refreshed
ends the keepalive for the session (`daemon/keepalive.rs`). Observed live
(2026-08-21): a mandatory-reasoning endpoint reasons on a `max_tokens: 0` ping —
~4k thinking tokens per ping, billed as output — turning the free refresh into a
recurring charge. A runtime observation rather than a provider gate, so it also
catches any future endpoint that stops honoring the contract; the offending
ping's usage is still recorded.

The test is a ratio, not an offense count: output is tolerated while
`discard_tokens × 100 < prefix_tokens`, where prefix is the ping's own reported
prompt total (input + cache read + cache creation, however the endpoint split
it). The constant is the price comparison with the prices cancelled out — an
output token costs roughly 50× a cache-read token on Anthropic and 25–100× across
providers, times a margin factor of 2. Order of magnitude is the intended
precision; a dollar computation is not available at the decision point, since
prices are only known retrospectively and subscription sessions have no dollar
surface at all. The ratio self-scales, so a 500k prefix absorbs a few thousand
reasoning tokens while a small prefix absorbs none and a runaway generation fails
at any size — which is what keeps a mandatory-reasoning endpoint usable instead of
disabling keepalive on its first ping. Pinging with thinking disabled is not an
escape: changing thinking parameters invalidates message-level cache on Anthropic,
so that ping would refresh nothing.

A failed ping is classified by HTTP status (`api::KeepaliveError`, which carries
the status structurally rather than in its message). An invalid-request-class 4xx
is **terminal for the session's keepalive** — the upstream rejected the request
itself, so the next tick would be rejected identically, and retrying only reprints
the same line for the life of the pause. 408 and 429 describe the moment rather
than the request; they, 5xx, and network failures keep the print-and-retry
behavior.

The prefix bytes sent by a keepalive must be byte-identical to what a real turn
sends, or the ping writes a new cache entry instead of refreshing the live one.
`Client::keepalive` takes the same `messages`, `tools`, and `tool_choice` the
loop uses, and reuses the same `system` prompt from the client, so identity is
structural rather than a fragile string comparison.

### The ping ends on a user turn

Before placing the breakpoint, the loop drops trailing assistant messages until
the tail is a user-role message (tool results project as user role), on every
dialect. A session idles in exactly the state that produces a model-turn tail: a
settled session's log ends with the closing prose, and a session parked at an
approval gate ends with the `Assistant` entry carrying the awaiting `tool_use`,
its result not yet in the log. Anthropic accepts a request ending with a model
turn; Google-backed endpoints reject it ("Requests ending with a model turn are
not supported"), so an unlopped ping fails on those endpoints during precisely
the idle it exists to cover.

Lopping loses no cache: the trailing model turn was the last response's *output*
and has never been sent as input, so the live cache entry already ends at the
previous trailing breakpoint, and the lopped ping is a full read of it. The only
forgone benefit is on Anthropic, where an unlopped ping also *wrote* an extension
covering the model turn — lopping moves that write to the next real turn, which
was going to pay it anyway. A log that lops empty (nothing but model turns, or
nothing at all) skips the ping rather than sending an empty `messages` array.

This is a transport-side transform on the ping only. The projection is untouched
and its byte-stability pins do not move.

### Metrics cell

`Session` holds a transport-only metrics cell: the latest `Usage`, cumulative
totals, two monotonic `Instant`s, and one wall-clock stamp. Two clocks, two
jobs: the `Instant`s drive *interval* logic (when to ping, when the keepalive cap
is reached), where a monotonic clock is correct because an NTP step must not move
a deadline; the wall stamp is what clients read. Neither reaches the projection.
`GET /state` carries a `usage` block with `last`, `cumulative`, and
`last_request_at` — an **absolute** instant rather than an elapsed count, because
a field that ticks with the clock would make the level-triggered `usage` topic
re-emit forever (docs/control-api.md). Clients subtract from their own clock.

The fork peek's usage is folded into the metrics cell's totals. It does not stamp the
cache-touch clock: `usage.jsonl` records a peek the same way whether it ran on the
agent's client or a separate triage client with its own prefix, so only turns and
keepalive pings count as touching the main prefix (`RequestOrigin::touches_main_prefix`).
Leaving shared peeks out can only make the cache look colder than it is.

### keepalive_loop: two clocks

`keepalive_loop` is spawned by the session runtime. It fires `Client::keepalive` when the
cache nears eviction at idle.

Two clocks keep this correct:

- **Cache-touch clock** — stamped by `record_request` for real turns and by
  `record_keepalive`. Tracks when the main prefix was last read; the cache is
  believed cold once it is older than the provider's cache lifetime
  (`keepalive::believed_cold_since`, backlog: cold-session.md).
- **Real-turn clock** — stamped only by `record_request`. Tracks when a real
  turn last occurred.

Both are seeded from `usage.jsonl` when a session's host attaches the record, from
the last turn or ping and the last request that was not a ping. A restarted host
therefore keeps pinging a cache its predecessor kept warm, and a resumed session's
cache is judged by when it was last read, not by when the host started.

A keepalive refreshes the TTL (`record_keepalive` stamps the touch clock) but
does not reset the give-up deadline (the real-turn clock is untouched). This
prevents a keepalive loop from keeping itself alive forever.

The loop pings when the touch clock exceeds the configured interval
(`[transport] keepalive-interval`, above) and stops once the real-turn clock exceeds `CAP` (30
minutes of genuine idle). Past that threshold, one cold cache read on resume
is cheaper than indefinite pinging. During active work, real turns keep the
touch clock reset and the keepalive never fires.
