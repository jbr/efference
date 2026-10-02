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

Each result is rendered as its own block, headed by a short **label** (`result r1 ·
bash`) and **numbering from 1**. A single continuous coordinate system across results would invite the model to keep
lines from one result under another's verdict — a mis-attribution that renders exactly
like a correct verdict, so nothing downstream could detect it. A multi-result verdict is
correspondingly an array keyed by label; a verdict naming a label that was never offered
is dropped, the same harness-side guard applied to notification ids and roster names. A
**single-result** gate — the overwhelming majority — asks for no label at all; see
[the two answer shapes](#two-answer-shapes-and-one-scoped-ask) below.

Labels are `r1`, `r2`, … over the gate's offered results in call order, assigned and
mapped back to calls harness-side (`distill::labels`); the provider's call id stays on
the `tool_result` pairing and never appears in the view or the ask. Keying by call id
asks the model to copy back a 24–36 character opaque string, which verdicts got wrong
by dropped or altered characters under both UUID-shaped and `call_…` ids; each miss
failed coverage and withheld a result the model had decided on.

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

- It gets no label and is not in the gate's `offered` list, so the ask never mentions it
  and coverage never requires a verdict for it.
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
- **Thinking off** (`Client::without_thinking`) unless `--triage-thinking` or
  `--triage-effort`: the peek's reasoning is discarded with the tail, so it is latency
  and tokens per gate. How "off" is spelled follows the model's metadata
  (`ModelCapabilities::reasoning_mandatory`, [transport.md](transport.md)): a model
  whose reasoning is optional gets the explicit `thinking: disabled`; one whose
  reasoning is mandatory — or whose capability source declares nothing either way —
  has the field omitted and, on OpenRouter, `reasoning: {enabled: false}` beside it. `--triage-effort <level>` turns the model down instead
  of off, validated against its declared levels like `--effort`.
- **A peek-sized output cap** (`PEEK_MAX_OUTPUT`, 16k, or the model's cap if
  smaller): a verdict is one small call, and reserving the model's whole cap against
  the window brought the overflow forward to a 66k prompt on a 131k model.
- **None of the agent client's** effort, temperature, or upstream pin; its own
  capability lookup, so its output cap and context window are its own.

**The idle triage is not delegated.** The standalone triage an idle session runs when a
notification arrives ([inbox.md](inbox.md)) sends the agent's client, the agent's full
`tools[]`, and `auto` whether or not a triage model is configured
(`ForkPeek::idle_envelope`). That ask decides whether the session wakes at all, and a
wrong dismiss there is silent: nothing re-raises it until something else wakes the
session. It runs on the agent lineage, at the agent's thinking, reading the cache the
keepalive holds warm through idle; a separate model's reflexive answer is kept for the
per-result peeks, which are the volume. Observed before this split: a haiku-class
triage model with thinking off answered the idle ask in 23–32 output tokens and gave
opposite verdicts on the same self-scheduled timer exit two cycles apart
(journal/2026-09-14-idle-triage-on-the-agent-lineage.md).

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
boot uses (`ForkPeek::for_triage_model`), the session's own preamble context and
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
in `src/distill/test.rs`. Where the result header carries the call's `intent`, the note
answers it, and when the output does not contain what the intent asked for the ask prefers
saying so over summarizing what the output does contain.

**`intent_feedback` — the one channel back about the intent.** Optional beside `note` in
both answer shapes, absent nearly always. The ask scopes it to the case where the intent
gave the peek nothing to decide against — it restated the arguments, or named no question
the output could answer — and to one clause saying what it lacked, so the next intent is
one the peek can act on. It is about the intent and never the task: no suggesting what to
call next, no saying what the output means. Two reasons it is a field and not a clause of
the note. The note's rules are about the *output*, and this is about the *channel*; mixing
them blurs the rule with the worst failure mode, assertion past the output. And the
preamble's intent guidance is a frozen soft constraint that decays across a long
mechanical run, while this is a renewal delivered in-band at the point of failure — the
same mechanism as re-issuing the ask after a wrong-tool peek. The spotter's persuasion
concern (the field being written for a gate that can punish, [permissions.md](permissions.md))
does not apply: triage cannot punish, so there is nothing to plead to. Its emission rate
per session and per triage model is a reading of how often intents are degenerate.

- **One result** — top-level `keep`/`note`, **no label**. Naming the one result whose
  output is right there is transcription with no decision in it.
- **Several results** — the `results` array, one entry per `result` label. Here the
  label is the only attribution there is, so it is asked for and the gate's labels are
  listed in the ask.

Neither field is `required`; the harness accepts either shape for either size, because
nothing enforces the schema and a parse failure costs a withheld result. On a
single-result gate two id-free shapes resolve unambiguously and are adopted: a bare
top-level `keep`, and a **lone** `results` entry whose `result` doesn't match, which is
re-keyed onto the one offered label — with a single target there is nothing to
mis-attribute *to*, so rejecting a decided verdict over a mistyped label withholds output
for nothing. Neither applies once several results are in flight.

**The ask states what is out of scope.** Both shapes carry a line disowning the
transcript behind them. The earlier phrasing — "every `[call id]` **above** must be
answered" — is unbounded, and every prior `tool_result` on the canonical wire already
carries a `[seq]` block-addressing tag (below) in the same bracket syntax the result
headers then used for call ids. Observed against a live model on a long transcript: it
answered by re-triaging the whole conversation — twenty-odd entries keyed by `[seq]`
tags, its own dialect's internal tool names (`functions.read`, `multi_tool_use.parallel`),
and fabricated ids — with the gate's own id usually absent, so coverage failed, the retries
exhausted, and the result's output was withheld. The pull scaled with transcript length:
clean early in a session, then most gates failing. Gates that happened to pass were no
better, deciding the real result inside a pile of invented ones. Naming the count, the
labels, and the exclusion is what bounds it.

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

### A gate that cannot triage holds

The gate never shows the agent a triage failure, and never falls back to the untriaged
raw. It retries until it has a verdict or the operator stops the turn:

- **Upstream failures** — an aborted peek stream, or a peek request that failed
  transiently — are retried on the main loop's budget and schedule
  ([transport.md](transport.md)). The fork's tail is throwaway, so the retry re-sends
  the same bytes over the warm prefix.
- **Unusable verdicts** — the wrong tool, no tool, malformed or incomplete input — get
  bounded in-fork retries (`PEEK_ATTEMPTS`), and when those are spent the fork is
  dropped and a **fresh fork** starts from the ask alone (`PEEK_FORKS`, three), with
  none of the rejected turns in its tail. A fork that has argued itself into a wrong
  shape tends to stay there.
- **Still no verdict** — the upstream schedule ran out or the upstream refused, or
  every fork came back unusable — and the loop **holds** (`agent/gate.rs`). The batch's
  results stay in the loop, unappended; the session stalls with
  `StallReason::TriageUnavailable` (upstream) or `StallReason::TriageDefect` (every
  fork unusable, which is a harness defect and is logged at error level as one) while
  the loop keeps running. The operator's continue runs the gate again from the top;
  the operator's interrupt ends the turn with each result withheld as the interrupt's
  *after* juncture withholds one — `WireState::Elided`, nothing that says triage
  failed, and the operator's own message next on the wire
  ([control-channel.md](control-channel.md)).

Why not an error on the wire: withholding each result as
`WireState::TriageFailed`, a hard error saying the call ran and its output was
withheld. Every wording of that message was a problem for the agent to solve. It had
to decide whether to re-issue a call whose side effects already stand — a blind
re-issue of a stateful command has turned a passing result into a failing one — and
agents read it as their own tool failing even when it named the harness ("That
listing hit a harness failure. Retrying.", `.efference/still-cliff`, 2026-09-28).
Failing a harness step is the harness's problem, so the harness waits on it rather
than handing it to the agent. The variant stays in `WireState`, no longer written, so
logs that carry it still load and project as they did.

Why not keep-all: `WireState` is fixed once, so an uncurated result would sit in the
cached prefix for the rest of the session with every later turn paying for it.

A hold is visible to the operator — the `stall` topic, the TUI's status bar, the web
composer's readout — but the results themselves are not yet, because nothing is on the
log until the gate decides their wire form (backlog/result-before-triage.md). A host
that stops during a hold loses them as it would mid-tool.

**Every attempt leaves a breadcrumb.** The `Triage` records the attempts it took in
`retries` — the model's unparseable `retain` input, the tool it called instead, or the
transport error, each stamped with its fork and attempt, plus a `held: …` line for each
hold the operator retried. They land on the `Triage` of the attempt that finally
succeeded, so a recovered failure is still in the corpus. Without them, a malformed
verdict, a wrong-tool call and a 429 are indistinguishable in the log after the fact. A
**coverage** rejection names what the verdict did answer as well as what it missed, for
the same reason: "answered nothing" and "answered twenty-six ids that were never
offered" are different defects, and the second is a prompt-scoping failure the first
gives no hint of — a session's worth of gates failed that way with nothing in the log
to say so.

**A verdict may arrive in parallel calls.** The shared envelope allows parallel tool
calls ([README.md](README.md)), so a peek may answer a batch in several `retain` calls
— one per result, say. They are merged into one verdict before coverage is checked.

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
    (`WireState::Distilled(Distilled { keep, note, intent_feedback })`), not at append — the wire
  form is re-derived from `raw` plus the verdict each turn, so the kept bytes are never
  stored twice. When a note survives, `render_hunks` prefixes it with the `⟢`
  provenance sigil — the same marker surfaced-context lines carry (`⟢ context ·
  …`) — so the note is honestly signed as harness-curated output without invoking
  mechanism vocabulary. An `intent_feedback` follows the note under the same sigil
  with a fixed `re your intent:` lead, so the agent reads it as the curator's remark
  about the channel rather than about the output. The sigil is render-layer only; the
  fork produces bare strings and is unaware of how they're rendered. Implemented in
  `src/project.rs`.

## The verdict is the stored form

`WireState::Distilled` carries the **verdict** as one `Distilled` struct (the `keep`
ranges, optional `note`, and optional `intent_feedback`, the last serialized only when
set so recorded logs are byte-unchanged by its arrival), not the rendered bytes. The
struct is what `render_distilled` takes and what the builders construct, so a field
added to the verdict is one setter and one render clause rather than a change to every
site that names the variant. Two payoffs from one move: the kept bytes are
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
`usage.retention` and the TUI session drawer renders it as `kept  55% · 13k/24k lines`
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

These tags once shared their bracket syntax with the peek's call-id headers, which is
what made an unbounded peek ask resolve against them; the ask is scoped rather than the
`[n]` syntax changed, since `[n]` is the handle the agent, the TUI, and the control plane
all use ([two answer shapes](#two-answer-shapes-and-one-scoped-ask)). The peek's headers
now carry unbracketed `r1` labels.

The **stable `seq`** is used, not a positional wire-index. A positional count
renumbers on elision or consolidation, which would bust the cache. `seq` is
immutable and survives compaction; it is a property of the log, and `/log`
plus `/events` already serialize it. A `BlockId` alias can later hide the
representation (`seq`, session-prefixed `seq`, short-sha) if needed. Tagging every
entry uniformly (vs. only tool results) is tracked in the backlog.
