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
like a correct verdict, so nothing downstream could detect it. A multi-result verdict is
correspondingly an array keyed by `call_id`; a verdict naming an id that was never
offered is dropped, the same harness-side guard applied to notification ids and roster
names. A **single-result** gate — the overwhelming majority — asks for no id at all; see
[the two answer shapes](#two-answer-shapes-and-one-scoped-ask) below.

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

## The known-result-shape rule — what the fork is not asked to decide

Size is not an exemption; **shape** is the one that is. A result whose value is a
machine-precise handle the model must reproduce exactly cannot pass through a
summarizer: a paraphrase preserves the meaning and destroys the function, and the
information that this result is unsummarizable is not present in the result. The peek
judges such a result reasonably — a short refusal is exactly the boilerplate the gate
compresses — and the paraphrase is silently useless.

So the decision is taken away from the fork rather than delegated to a prompt.
`src/distill/handles.rs` holds one entry per known handle-bearing shape, matched as a
sentinel plus a fixed-width hex digest anywhere in the result text. The shipped entry is
fs-mcp's refused-`write` retry token, `write-retry-` followed by 16 hex digits: a `write`
refused because the path exists (or because `overwrite` and `append` were both set)
preserves the rejected body server-side under that token and tells the caller to repeat
the call with the token as `contents`. The digest width and the hex alphabet are what
keep the match narrow — text that merely mentions the sentinel is not a handle. A second
handle-bearing format is one more entry in that list, not another mechanism.

A matched result is **bypassed**:

- Its call id is not in the gate's `offered` list, so the ask never mentions it and
  coverage never requires a verdict for it.
- Its block in the peek view is a stand-in — the call must still be answered or the
  request is invalid, but its output is not shown, since no verdict can change it.
- Its wire form is `WireState::Raw`: the whole body reaches the model byte-for-byte.
- The `Triage` records `origin: Fallback` with a `raw_keep` breadcrumb naming the shape,
  which is what distinguishes a bypass from a failed gate in the log after the fact. Its
  lines count toward neither side of the retention ratio: nobody triaged them.

When **every** result in a gate is bypassed the peek is not sent at all — the skipped
verdict is also a skipped round-trip. The gate-scoped riders wait for the next gate; an
undispositioned notification stays active and is re-shown there ([inbox.md](inbox.md)).

Two alternatives were rejected. **Prompting the fork** to preserve identifiers is exactly
the instruction a summarizer follows unreliably, and the failure is silent. **A
tool-declared not-distillable property** is the general answer if a third-party tool ever
mints handles, but it is a new trust surface — a tool that over-claims it defeats triage
wholesale — so it waits for that case. Removing the handle from the tool instead (having
`write` default to overwrite, or confirming a refusal by path and flag alone) is also not
taken: the token disambiguates *which* write failed when several are in flight, which a
path and a flag cannot.

## One request shape — the shared envelope

The peek sends the **same request envelope as an agent turn**: the turn's full
`tools[]` (the world tools plus the `retain`/`triage` schemas, one shared array built
by `agent::tool_definitions`), `tool_choice: auto`, and consequently the same thinking
policy. Nothing cache-keyed differs between the two shapes, so the fork is the same
conversation continued with a different tail and reads the agent lineage's transcript
cache directly. The alternative — a peek-only tools array and a forced `tool_choice` —
keys a **second cache lineage** over the same transcript: on Anthropic the
messages-tier cache is keyed on `tool_choice` and the thinking setting, and everywhere
a differing tools array splits the prefix, so the fork's whole transcript re-billed at
the cache-write price on every peek ([log-and-projection.md](log-and-projection.md)).
What forcing bought is re-bought harness-side:

- **The peek schemas are static.** `retain`/`triage` are zero-argument builders; the
  `inbox`/`background`/`surfaced` fields are unconditional, with self-contained
  descriptions ("if a section named X appears above…"), because a schema that varied
  with session state would re-key the envelope mid-session. Invented dispositions were
  already dropped harness-side, so conditionality guarded nothing the filters don't.
  The **tail instruction** is under no such constraint — it is discarded with the fork —
  so what varies per gate (the ask's scope, its answer shape, the offered ids) lives
  there, and the schema carries every shape unconditionally with nothing `required`.
- **A standing permission clause** on both descriptions — "do not call this tool
  unless asked to do so by the harness." Permission-phrased, not identity-phrased
  ("harness-invoked only"), because the same model reads the standing description and,
  later, the peek's tail ask: a permission clause resolves as "now is when"; an
  identity claim contradicts the ask and costs deliberation.
- **An un-asked call** — the agent calling `retain`/`triage` on a normal turn — never
  enters the permission cascade; the loop pairs it directly with a stable error result
  ("not run — nothing is being asked of `…` right now; continue with your task").
  Single-call turns make this a wasted round-trip, not corruption, and the stable
  phrase keeps the un-asked rate greppable from the logged raws.
- **A non-compliant peek** — the wrong tool, no tool, a malformed verdict, one that
  leaves an offered result unanswered (coverage is part of validity), or one whose
  `keep` range lies wholly outside the shown line numbers (bounds are too) — gets a
  bounded **in-fork retry**: the non-compliant assistant turn is appended to the
  throwaway tail verbatim (thinking blocks included), each of its calls is answered
  with an error `tool_result` carrying the why, the ask is renewed, and the request
  goes again — a warm prefix re-read on the same lineage. Exhaustion is a hard error
  (below), never a keep-all. Every non-compliant attempt — **including ones a retry
  recovered from** — is recorded attempt-stamped on the gate's `Triage` (`retries`)
  and rendered as a `⚠ triage retry` annotation in the TUI transcript: a runtime
  triage failure is a harness defect signal, never silently accepted, and the
  compliance record is a fold over that field. Implemented in `src/distill/peek.rs`.

Un-forcing also lifts the forced-choice-excludes-thinking constraint
([transport.md](transport.md)): the fork may think, and the thinking is discarded with
the tail.

## The separate-model peek

`--triage-model` names a model other than the agent's and the peek runs on its own
`Client` (`ForkPeek::own`) instead of borrowing the agent turn's (`ForkPeek::shared`,
the default, and what an omitted or equal `--triage-model` boots). The shared
envelope above exists to keep one cache lineage; a second model is a second
lineage over the same transcript whatever the envelope does, so the own client
drops the parts of it that only served that:

- **Its own system prompt** (`env::triage_system_prompt`): the triage model's real
  identity line, a persona that says once that the assistant turns were produced by
  the agent, named by model, and are to be read as the model's own working context,
  then the session's `[context]` block verbatim. No curation preamble, role section,
  or unmounted list — those describe the agent's perimeter.
- **A `tools[]` of only `retain` and `triage`**, and the asked-for tool **forced**
  (`tool_choice: {type: tool}`), so a wrong-tool answer cannot happen. The first run
  under `auto` opened five of eight gates with an invented `read` or `bash`;
  `--triage-auto-choice` restores `auto` as the ablation, and is what a Claude
  triage model needs if it is to think.
- **Its own sticky key** (`Endpoint::for_triage`, a `-triage` suffix), for the same
  reason as the spotter's ([transport.md](transport.md)).
- **Thinking off** (`Client::without_thinking`) unless `--triage-thinking`: the
  peek's reasoning is discarded with the tail, so it is latency and tokens per gate.
  On OpenRouter a foreign model gets `reasoning: {enabled: false}`, since the
  `thinking` field is omitted for it and OpenRouter's default is on.
- **A peek-sized output cap** (`PEEK_MAX_OUTPUT`, 16k, or the model's cap if
  smaller): a verdict is one small call, and reserving the model's whole cap against
  the window brought the overflow forward to a 66k prompt on a 131k model.
- **None of the agent client's** effort, temperature, or upstream pin; its own
  capability lookup, so its output cap and context window are its own.

The prefix is the same canonical projection either way, so the own lineage is warm
across gates: each peek writes the newest assistant turn and reads the rest, at the
triage model's rate. The **tail ask is unchanged** — first person, "the call you just
made" — and byte-identical between the two configurations, so a replayed gate
differs only in model and head. The `+third-person` ask variant is the framing
alternative, an ablation, not a default. The `note` field's ask says whose note it
is: a note to self, read on the agent's next turn in place of the dropped lines,
which holds whichever model writes it.

**Evaluating a candidate offline.** `efference dev peek <session> --triage-model X`
replays every recorded gate under the separate-model shape — the same constructor
`serve` boots (`ForkPeek::for_triage_model`), the session's own preamble context and
model name in the triage system prompt — and compares each draw's verdict against the
**recorded** one, line for line after the cap. The reference is what the session's own
peek chose to see, not an operator's idea of the right keep: the agent doing the work
is the only reader who knows what it needed. The summary reports the kept fraction for
both, over-retention (lines the candidate kept that the reference dropped) and
under-retention (the reverse) as separate fractions since they are different failures,
exact-match count, and `none` verdicts against a reference that kept something. A
table sorts every compared result by disagreement with both notes beside it; `--out`
writes every row as JSON. Per-gate judgment against the same prefix is what this
measures; a candidate triaging from turn one would build a different prefix, and only
a live session measures that.

What is *not* built: the own client's retry and reservation state is not reported
on `GET /state`, and nothing acts on the triage model's context window being
smaller than the agent's (backlog/distiller-experiment.md).

## Two answer shapes, and one scoped ask

The schema is static while gate size is not, so `retain` carries both shapes and the
tail instruction (`retain_instruction` in `src/distill/prompt.rs`) names which one this
gate wants.

**The ask is shaped against the pull of the turn it interrupts.** It carries its own
`═══` banner, like every other section of the view; it opens on the imperative and
repeats it last; it states that this is a triage turn and not a work turn; and the
per-field asks (`keep`, `note`, and whichever of `inbox`/`background`/`surfaced` this
gate carries) are a scannable block rather than prose appended after the fact. What
shipped first was one unbroken paragraph assembled from four separately-authored
fragments, opening on a description and closing on a tangent — the only *instruction* in
the view formatted as a trailing note about the sections above it. A session answered 29%
of its gates with a work tool instead of a verdict, usually the tool it went on to call
on the next canonical turn, at a rate that climbed with session depth; every structural
signal in the request said "ordinary tool result", including `tools[]` carrying the whole
world toolset under `tool_choice: auto`.

Measured by replaying a recorded session's gates one feature at a time
(`efference dev peek`), **no single one of those is necessary** — ablating any one of the
banner, the leading/closing imperative, the triage-turn stance, or the field block leaves
the failure rate where it is, while ablating all four costs about twelve points. The
shaping is redundant rather than resting on a particular sentence, which is the useful
thing to know before editing it.

The ask also states that **the verdict is the `retain` call's arguments and prose is not
read**. That one is separately attributed: measured against the all-features-off variant,
where there was headroom to see it, it moved the failure rate from 14% to 5%. Its target
is a specific failure — a peek writing a correct verdict as prose *and* calling a work
tool for the next step. An instruction to "respond with a tool call" would not have
touched that, since a call was already being made; what was missing was that the prose
channel isn't read.

The closed-toolbox sentence — "no other tool will run until you have called `retain`" —
is a statement of fact, not a discouragement: a non-`retain` call is answered with an
error `tool_result` and the ask is renewed (below). `note` asks for what the output
showed *and only that*, because a peek once turned a raw `unknown tool: ferritin` into
"ferritin is not available as a CLI tool in this environment" — false, and since the raw
is discarded, the invented claim was the only thing that reached the canonical wire.
Dropping the wrong lines is the failure this gate is built against; asserting past the
output is worse, because nothing downstream can detect it.

Two further clauses answer the same failure in its second observed shape, where the note
was true of the *raw* and unsupported by the *kept lines*. A `search` for `^}` returned 19
matches; the verdict kept one line — the second-to-last match — and the note stated a fact
about the last one. Both halves reached the wire disagreeing, and the session re-verified
with `wc -l`/`tail` before it would use either, which is the cost: an unauditable summary
is re-checked on every later result too. So `note` is asked to be grounded in what
*survives* rather than in what was shown ("if the note turns on a particular line, that
line belongs in `keep`"), and `keep` is told that a partial keep of an enumeration — a
match list, a listing, a candidate set — reads afterwards as the whole set, so either the
enumeration is kept whole or the note says how many of how many survived. That is the case
where a lean verdict changes the answer rather than shortening it. Both clauses are pinned
in `src/distill/test.rs`.

- **One result** — top-level `keep`/`note`, **no call id**. Naming the one call whose
  output is right there is transcription with no decision in it, and it is the single
  most failure-prone token in the response.
- **Several results** — the `results` array, one entry per `call_id`. Here the id is the
  only attribution there is, so it is asked for and the gate's ids are listed verbatim
  in the ask.

Neither field is `required`; the harness accepts either shape for either size, because
nothing enforces the schema and a parse failure costs a withheld result. On a
single-result gate two id-free shapes resolve unambiguously and are adopted: a bare
top-level `keep`, and a **lone** `results` entry whose `call_id` doesn't match, which is
re-keyed onto the one offered id — with a single target there is nothing to
mis-attribute *to*, so rejecting a decided verdict over a mistyped id withholds output
for nothing. Neither applies once several results are in flight.

**The ask states what is out of scope.** Both shapes carry a line disowning the
transcript behind them. The earlier phrasing — "every `[call id]` **above** must be
answered" — is unbounded, and every prior `tool_result` on the canonical wire already
carries a `[seq]` block-addressing tag (below) in the same bracket syntax as a
`[call id]` header. Observed against a live model on a long transcript: it answered by
re-triaging the whole conversation — twenty-odd entries keyed by `[seq]` tags, its own
dialect's internal tool names (`functions.read`, `multi_tool_use.parallel`), and
fabricated ids — with the gate's own id usually absent, so coverage failed, the retries
exhausted, and the result's output was withheld. The pull scaled with transcript length:
clean early in a session, then most gates failing. Gates that happened to pass were no
better, deciding the real result inside a pile of invented ones. Naming the count, the
ids, and the exclusion is what bounds it.

## The `retain` verdict

The fork asks for a single structured tool call — the ask rides the tail
instruction, never a forced `tool_choice` (see the shared envelope above) — so there
is no freeform parsing. The verdict is one of three forms:

| form | meaning |
| --- | --- |
| `retain(all)` | every line survives |
| `retain(lines=[40-52, 310-318])` | these 1-based inclusive ranges survive |
| `retain(none, note="…")` | nothing survives; the one-line note is the breadcrumb |

`retain(none)` is not silent: the note stays on the canonical wire so the agent
doesn't re-run what it just disowned — inline dead-end shedding at the source.

An aborted peek stream is retried first, on the main loop's budget and schedule
([transport.md](transport.md)) — the fork's tail is throwaway, so the retry re-sends
the same bytes over the warm prefix. When those attempts are exhausted, when the
request itself fails, or when the in-fork retries are exhausted without a usable
verdict, the gate **withholds the output as a hard error** — there is no keep-all
fallback. Each result's wire form becomes `WireState::TriageFailed`: an error
`tool_result` (`is_error` forced true) saying the output was withheld and why, and
that the call itself ran to completion — so the agent treats it exactly like a failed
call, deciding whether to re-issue it with side effects in mind, and the operator sees
the same error in the transcript. The message attributes the failure to the harness
rather than to the call, and makes the re-issue advice conditional on re-running being
safe: agents have read an unattributed triage failure as their own tool failing and
adapted a tool-use strategy that was never the problem, and a blind re-issue of a
stateful command has turned a passing result into a failing one. The full untriaged raw is never the fallback:
`WireState` is fixed once, so an uncurated result would sit in the cached prefix for
the rest of the session with every later turn paying for it. `raw` stays in the log
(the TUI can flip the result to raw), and the `Triage` records the same **breadcrumb**
in `raw_keep`: the model's unparseable `retain` input, the tool it called instead, or
the transport error — plus which attempt gave up. Without that, a malformed verdict, a
wrong-tool call and a 429 are indistinguishable in the log after the fact. A **coverage**
rejection names what the verdict did answer as well as what it missed, for the same
reason: "answered nothing" and "answered twenty-six ids that were never offered" are
different defects, and the second is a prompt-scoping failure the first gives no hint
of — a session's worth of gates failed that way with nothing in the log to say so. The message
is rendered by one shared function (`render_triage_failure`, stable
"result withheld — triage failed" prefix) so the wire and the TUI say the same thing.

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
  the coordinate system `retain(lines=…)` addresses. Implemented in `src/distill/prompt.rs`.

- **`render_hunks`** (the canonical wire form) — a `@@ lines a-b @@` header per
  contiguous kept span, then those lines verbatim. One header per range (not per
  line); gaps between hunks are implied by the jump in headers, as in a unified
  diff. A keep-all result is one header plus verbatim body, approximately equal to
  raw. This is the form for a reader that has only text; the TUI resolves the same
  verdict through the shared `distillate` and renders the kept lines with their
  numbers in a gutter instead ([tui.md](tui.md)). Both go through `distillate`, so
  the two views can differ in form but never in *which* lines they show.
  `render_hunks` runs at **projection time** over the stored verdict
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
| `origin` | `Model` (a real `retain` call) or `Fallback` (the peek failed — the result projects a withheld-output error) |
| `raw_keep` | the model's exact `keep` string, pre-parse — the audit breadcrumb |
| `inbox` | per-notification dispositions chosen in this peek, by arrival `seq` |
| `retries` | one attempt-stamped breadcrumb per non-compliant peek attempt, recovered or not — the durable compliance record (gate-scoped; rides the last `Triage` of a batch, like `inbox`) |
| `thinking` | the text of the peek's thinking blocks, one entry per attempt that thought (gate-scoped, like `retries`) |

The fork runs under the same thinking policy as an agent turn (the shared envelope above),
so most gates think. That thinking is recorded on the `Triage` entry and **never
projected** — it is what answers "why did *that* verdict happen", where replay
(`efference dev peek`) answers "how does the fork reason about this class of result". Text
only: a thinking block's `signature` is what the API needs to replay it, and the fork's
tail is discarded rather than replayed. It is operator/developer observability, with no
client surface.

A failed peek is not erased — its withheld-output error is visible on the wire — but
`origin` still classifies *what kind* of failure it was, which the rendered artifact
cannot say. `project` reads `Triage` only as audit (it emits pinned-notification blocks
from `inbox`) and skips it otherwise. Every fork decision is reconstructable from
the log — including ones only ever visible inside the throwaway branch — for
observability and distiller-quality work, with none of the fork's context touching
the wire or the cache.

## Retention — how much of the peek survives

`Retention` (`src/project/retention.rs`) folds the log into cumulative
`kept_lines` / `shown_lines`, the lines the peek cap dropped before triage saw them,
and a count of results whose output a failed gate withheld. `GET /state` reports it as
`usage.retention` and the TUI sidebar renders a `kept 55% · 13k/24k lines` line
([tui.md](tui.md)).

Two properties make the fold the cheap kind. It reads **only** `ToolResult` entries —
`raw.content` and `wire` are both on the one entry, so it joins nothing and resolves
each verdict through the same `distillate` the transcript renders from, rather than
becoming a second definition of "kept" that can drift from what the gutter shows. And
it is **monotone**: the log is append-only and a result's `wire` is fixed at append, so
a caller folds from a cursor and never invalidates. The daemon keeps that cursor, which
is why a resumed session reports the whole session's ratio instead of restarting at
zero — this is derived from the canonical record, unlike the transport telemetry beside
it in STATS.

A withheld result is **not** low retention. It retained nothing because nothing was
recorded, so folding it into the ratio would make a failing gate read as excellent
compression; it is counted separately and stays out of both sides. Lines past the peek
cap are likewise excluded from both sides — nobody decided about them — but are
reported rather than silently absent.

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
  (those with a `path`) in the boot listing that no prior gate has surfaced — and
  `retain` gains an optional `surfaced` field over those names. The roster is a pure
  fold over the prefix; names double as the constrained output vocabulary, validated
  against the roster (invented names dropped). Each item renders through
  `context::render_item`, the **same** renderer the boot preamble uses, so an item met
  mid-session is spelled `<name path="…">summary</name>` exactly as it was spelled at
  boot. A bare `[name]` shipped first and under-described what an item *is*: a session
  met a listed skill that way and built a tool call out of the name rather than opening
  the file, then wrote the resulting `unknown tool` error into canonical context as a
  false claim about the tool's availability.
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

These tags share their bracket syntax with the peek's `[call id]` headers, which is what
made an unbounded peek ask resolve against them; the ask is scoped rather than the syntax
changed, since `[n]` is the handle the agent, the TUI, and the control plane all use
([two answer shapes](#two-answer-shapes-and-one-scoped-ask)).

The **stable `seq`** is used, not a positional wire-index. A positional count
renumbers on elision or consolidation, which would bust the cache. `seq` is
immutable and survives compaction; it is a property of the log, and `/log`
plus `/events` already serialize it. A `BlockId` alias can later hide the
representation (`seq`, session-prefixed `seq`, short-sha) if needed. Tagging every
entry uniformly (vs. only tool results) is tracked in the backlog.
