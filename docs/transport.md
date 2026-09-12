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
cannot be dispatched however the stream ended (observed 2026-08-28: a foreign
model sent `message_stop` after fragments cut mid-string), so it becomes the same
record-only partial, keeping the raw fragment as a string, and the caller's
abort-retry budget decides what happens next. Returning a parse error instead
spent none of that budget and put a serde message where the model could read it.
The dividing line from the max-tokens case is deliberate: a
cap-terminated response is *finalized* — it carries a `stop_reason` and usage
and the loop rightly commits it — where an unfinalized stream carries neither.
Finalized → commit; unfinalized → record-but-retry. One exception rides the
stop reason alone: a `tool_use` block in a `max_tokens` completion was cut
mid-arguments (a model stops at `tool_use` after a call it finished, never at
the cap), and whether the fragments parse is the upstream's doing — observed
2026-09-02, a cap-cut `write` arrived as a valid object with its `contents`
split across nonsense keys and no `path`, and was dispatched. So a finalized
completion carrying both `stop_reason: max_tokens` and a call is marked the
same record-only partial, whatever its input parsed to. A failure observed *after*
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
unless `--triage-thinking` or `--triage-effort` (`Client::without_thinking`: explicit
`disabled` for a foreign model whose metadata says reasoning is optional, the field
omitted plus OpenRouter's `reasoning: {enabled: false}` for one whose reasoning is
mandatory — `ModelCapabilities::reasoning_mandatory`, from the model entry's
`reasoning.mandatory`; [redaction-fork.md](redaction-fork.md)); the keepalive follows the same family split
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

**Boot validation, per provider** (`Provider::validates_effort_levels` →
`ModelCapabilities::check_effort`, called from boot). OpenRouter refuses a
configured level the metadata does not affirmatively declare, because it maps
an unsupported level to the nearest supported one **silently** — a session
could otherwise believe it set a knob that did nothing (observed 2026-08-29:
`--effort max` on GLM 5.3 flash, whose `default_effort` was already `max`, so
the flag was a no-op by coincidence). A model that declares no levels refuses
every configured level: nothing was affirmed, so nothing is sent. Anthropic's
metadata names no levels, so the check does not run there and a misspelled
level is the provider's own 400 on the first turn. The boot line prints
`effort: {level}` when one is set, and `effort: unset · provider default
{default}` when the metadata names one, which answers what an unset knob will
do without a probe.

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
replaying them. It makes at most four total attempts and has a 30-second
wall-clock budget; an HTTP `Retry-After` is honored only within that same
budget. The set is shared by both dialects and is a superset of what OpenAI's
error guide names as retryable (429 rate limits and `slow_down`, 500, 503
`server_is_overloaded`, each with `Retry-After` when present); the guide's
non-retryable 429s — exhausted credit, spend and usage limits — are the body
veto in `api/failure.rs` (below), and Codex CLI retries all 5xx the same way.
Both Responses profiles ride the same handler: the bearer header, the account
header, the `originator` attribution, and the absence of `anthropic-version` are
pinned over a mock upstream in `src/api/test.rs`, as are a 400 open returning
its body once, a 503 open retried and queued, and the ceiling cutting a
Responses stream mid-text.

The retry handler covers failures while opening a response, before an SSE stream
has yielded any deltas. A connection that has returned 200 and then dies
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
condition and the alternative on exhaustion is worse than waiting: idling with
"send a turn to retry" needs an operator, and an autonomous run has none. The cap
is what lets the count be large without the tail becoming a hang — uncapped
doubling over twelve attempts would sleep for over an hour. Because a marked entry projects to nothing, every retry re-sends
byte-identical messages and rides the still-warm cached prefix. On exhaustion
the loop returns to idle, leaving the failure in two places: the record-only log
entries (durable) and the `stall` state topic (below), which carries the attempt
count and the upstream's own words.

**Who is told what.** The model is never told about an upstream failure. If the
harness is in a position to describe the failure to it, the harness has already
recovered from it — and the description is not neutral: a serde parse error
delivered to the inbox is something the model then reasons about mid-task
(observed 2026-08-28). So the audiences are the **log** (a record-only entry per
failed attempt, for audit), **stderr** (live operator text), and the **operator's
state topics** (`upstream_retry` while a retry is in flight, `stall` once a
budget is spent). Nothing on these paths posts to the inbox.

That idle is **legible as a stall**, not only as an idle. The daemon stays up on
this path deliberately — the socket is alive and one turn recovers the session —
but the return to idle otherwise looks exactly like a finished run to a client:
`running: false` and no gate. So the loop sets the **`stall` state topic**
(control-api.md): the reason, the attempts spent, the instant it gave up, a lead
line naming what recovers it, and the upstream's words as `detail`. It is
cleared at the top of the next trajectory rather than on a successful
completion, so it reports the condition now. The topic is addressed to whatever
is supervising the daemon from outside, which is the rung that can actually send
the recovering turn — an inbox copy would be addressed to the model, which is
neither told about upstream failures nor able to act on one. Before the topic
existed, an earlier inbox notice was the only signal and a triage peek could
dismiss it, leaving a clean idle (observed 2026-08-28). Both clients render the
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
stream is retried up to `STREAM_ABORT_ATTEMPTS` times on the same capped
schedule, re-sending byte-identical messages over the warm prefix (its
tail is throwaway, so nothing has to be committed to arrange the replay). Only
on exhaustion — or a terminal failure, above — does the gate withhold. The surfaces used to disagree — an
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

An exhausted *open* failure (a non-2xx after the retry handler gives up) still
fails the current trajectory rather than the daemon, but no longer only to
stderr: the loop appends an empty record-only assistant entry carrying the
error body — Anthropic's 4xx bodies name exactly what was malformed, and the
log is the durable file where stderr is not — and sets `stall` with `reason:
"request_failed"` carrying that body as `detail`, before returning to idle. The
model is told nothing here either. The projection
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
different dialect, or in both. A list with no assistant turn yet belongs to no dialect
and encodes on either; `dev wire` with no provider declared renders in the recorded
dialect. A provider or model change within one dialect passes this check.

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
`parallel_tool_calls: false` (one call per turn), `tools[]` as `function` entries
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
`prompt_cache_breakpoint` markers — the backend rejects each of those with a 400
`Unsupported parameter` (measured 2026-09-10 on gpt-6-astra and gpt-5.6-luna
alike, so this is the backend's surface rather than one model's), so the arm's
capability profile reports implicit caching. Explicit mode is unreachable here,
and would be worse than unreachable if half-applied: `mode: explicit` with no
breakpoints disables caching outright. "What Codex CLI sends, and nothing it does
not" cuts both ways — the arm shipped for three days without `session-id`, which
Codex CLI does send, and paid for it in cache misses. Auth is the OAuth access
token Codex CLI caches in `~/.codex/auth.json` (`$CODEX_HOME` honoured) sent as
`authorization: Bearer`, plus `ChatGPT-Account-ID` from the same file and an
`originator: efference` header. efference reads that file and never writes it;
refreshing an expired token is `codex login`'s job (backlog/openai-provider/chatgpt-oauth.md).
Both profiles have a byte pin.

The Codex backend streams with **no `content-type` header** (verified with curl,
2026-09-06). `into_sse` refuses an absent header the way a browser `EventSource`
does, but the failed negotiation still owns the executed conn, so the Responses
dialect recovers the stream from it with `EventStream::new` when the error is
`UnexpectedContentType(None)`; a present non-`text/event-stream` value, and every
other dialect, stay on the open-failure path. Each response also carries
`x-codex-*` headers naming the plan, the 5-hour and 7-day usage windows as
percentages, and their reset times. `api/plan.rs` reads them off every response
the status handler sees — a 429 included, since the response that ends a window
is the one that most wants reading — into a cell the daemon publishes as
`usage.plan` (control-api.md): the plan type and each window's used percentage,
length, and reset as absolute millis. A response carrying none of the headers
retains the previous snapshot; one carrying any replaces it whole. The snapshot
is operational state, not a record: it never enters the log, so the captured-
header allowlist and the masker are not involved, and a resumed session starts
with `null` until its first Codex response.

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
rejection of `prompt_cache_breakpoint` above was observed on gpt-5.6-luna; OpenAI's
guide lists explicit breakpoints as a GPT-5.6-family feature on the platform, so it
is the backend, not the model, that is presumed to refuse them (backlog:
openai-provider/validation.md).

**Not on this dialect.** The `max_tokens: 0` keepalive is a Messages primitive; the
daemon never starts the loop on Responses (backlog: openai-provider/validation.md).
OpenAI exposes no per-generation cost endpoint, so cost tracking stays off. The
spotter model must be one its connection's metadata source knows: with no
`[defaults].spotter`, the main connection's cheap band applies
(`Provider::default_spotter_model`, gpt-5.6-luna here, claude-haiku-4-5 on the
Messages providers), and an explicit `--spotter-model` passes its own provider's
check. The catalog
(`api/provider/openai.rs`) is exact per model: GPT-5.6 and its sol/terra/luna
variants, and GPT-6 Astra, which takes no `none` effort and no `temperature` —
`ModelCapabilities::accepts_temperature` is what makes an explicit
`--temperature` against it fail at boot rather than on the first request. An
unknown slug is refused with the list this build knows; slugs are never
normalized, so `gpt-6.0-astra` is unknown.

## Providers

The request **body** is the dialect's encoding of the shared semantic projection —
Messages for Anthropic and OpenRouter, Responses for OpenAI — so the log, the
semantic fold, and the cache-frontier model are provider-independent. What varies
between providers is the **dialect**, **where** the POST goes, **how** it
authenticates, and **which metadata source** answers questions about the selected
model. All derive from one declared identity: `api::Provider`, a closed enum
(`anthropic`, `openrouter`, `openai`, `openai-codex`) grown in code as providers
are added. Identity is an input, never inferred from URL shape (sniffing fails for
proxies and custom URLs). `api::Endpoint` carries the derived `url` +
`auth_header` + `auth_value` alongside the provider; its `Debug` redacts the
credential and extra headers.

**Host-owned connections (`src/inference.rs`).** A host names its connections in
`~/.efference/config.toml`, read from the host's startup `HOME` — a secret-free
file: each `[connections.<name>]` declares a `provider` and where its credential
comes from (`auth = { kind = "env", key = "VAR" }` reads the named variable from
the host's startup environment, ordinarily supplied by `op run`; `auth = { kind =
"codex-cache" }` reads Codex CLI's `auth.json` once, under the host's `CODEX_HOME`
or `HOME`). Credentials load once at host launch, each connection independently:
an unavailable one carries its reason and fails only the sessions that select it.
Nothing re-reads a caller's environment, re-runs a secret command, or re-opens the
cache per session, and the host's environment is never overlaid onto a session's
snapshot or a tool child.

A session selects a model per role as `<connection>/<native model>`. A built-in
provider name is always a connection prefix, configured or not (`anthropic/…` on a
host without an `anthropic` connection names the missing connection, never a
native id on the default). Any other id, slashes included, resolves through
`[defaults].model`'s connection; a native id equal to a connection name must be
written qualified. Omitted `--model` takes `[defaults].model`; an omitted spotter
takes `[defaults].spotter`, else the main connection's cheap band; an omitted
triage takes `[defaults].triage`, else shares the main client. Defaults are
written fully qualified, so adding a connection cannot reinterpret one. All three
roles resolve before a session directory exists; a main/triage pair on different
dialects is refused there, naming both. Each role gets its own clone of the
connection's endpoint with its own sticky key, so two sessions on one connection
share a credential, not a client. The native id alone is what a request carries;
the connection name appears only in boot output and `session_identity`.

The dev verbs that dial a provider without a host (`dev wire`, `dev peek`, `dev
spotter`, `dev spotter-eval`) load the same file from their own environment and
resolve their `--model` through it; nothing reads a provider from environment
variables.

**OpenAI capabilities** come from a compiled catalog (`api/provider/openai.rs`)
rather than a metadata GET: the Models endpoint reports none of the limits the
boot path consumes. Entries are exact — `gpt-5.6`, `gpt-5.6-sol`, `gpt-5.6-terra`,
`gpt-5.6-luna` today — and an unknown model fails boot rather than inheriting a
family's limits.

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
terminating the turn at the cap before any text or call arrives (observed
live, 2026-08-21; the committed turn was thinking alone). A cap-terminated
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
`limit` is `context_length` less what the last request reserved — the wall the
prompt actually hits, the full window before any request — and
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
from, and the offline `efference dev cache` view are in
[log-and-projection](log-and-projection.md).

**Session cost telemetry.** Where the provider has a per-request accounting
surface, every recorded request (turn, peek, spot, triage, keepalive — plus
keepalive pings funnel through the same two record methods) spawns a
best-effort fetch of its **billed** cost, folded into a running total that
`GET /state` reports as `usage.cost = {usd, unpriced}` and the sidebar renders
as `cost $0.0421`. The one exclusion is an `aborted_attempt` line with all-zero
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
Anthropic-served models, over a minute observed for `meta/muse-spark-1.3`), so
the fetch backs off across six attempts (2s through 300s, `COST_FETCH_DELAYS`
in `daemon/metrics.rs`); exhaustion increments `unpriced`, so the total is a
legible undercount rather than a silent one. On direct Anthropic there is no
per-request cost surface (the Models API has no pricing; the Usage & Cost
Admin API reports realized org costs in daily buckets under a separate admin
key) — `usage.cost` is null there and the sidebar omits the line.

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
was previously invisible and died at the 5-minute TTL on any idle. This layer
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

The fork peek's usage is folded into the metrics cell because the fork reads the
shared canonical prefix and therefore slides the cache TTL.

### keepalive_loop: two clocks

`keepalive_loop` is spawned by the session runtime. It fires `Client::keepalive` when the
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
