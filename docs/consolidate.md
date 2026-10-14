# Trajectory consolidation

Consolidation replaces newly completed working material with an account for
continuation. `src/consolidate.rs` advertises the agent-called tool;
`src/consolidate/span.rs` selects inclusive ranges and measures projected tool
content. `src/agent/consolidation.rs` schedules opportunistic checks. Projection
lives in `src/project.rs` and `src/project/semantic.rs`.

## The working span

`MarkKind::Span { from, upto }` names the exact inclusive sequence range to
replace. Entries before `from` and after `upto` remain. Projection inserts the
account where that range ends, ahead of the first retained entry following it,
rather than where the mark was appended. Earlier accounts remain outside the
newly eligible span and are not normally summarized again.

The first span starts at the first working user/assistant/result entry, leaving
the preamble intact. An adopted span or main-reader head fold establishes the
next earliest reachable sequence: after its `upto`. A completion report or
question that the operator is responding to remains verbatim, as does a fresh
operator turn after that report. Intermediate operator messages inside the
range can be summarized by forward utility, with their scope preserved and
exact quotations where wording matters.

```
[prefix] [working trajectory] [report/question] [operator reply]
    becomes
[prefix] [account]            [report/question] [operator reply]
```

Selection retains the most recent report/question, or a newly delivered user
instruction after additional work. It refuses a range containing only retained
surroundings from an earlier consolidation. Whole assistant call batches and
all their results must be paired: no unanswered or split calls, pending verdicts,
or orphan results can enter the shared request. An ask check waits until its
reply and every sibling result have landed with their verdicts.

## Opportunistic checks

The loop considers a check at a delivered user turn, a trajectory-ending
completion, and after an ask's complete batch. Eligibility is the UTF-8 bytes
of ordinary tool-call argument JSON plus post-verdict projected result text
in the candidate range. `consolidate` calls/results are excluded. The accounting
uses production projection, including result-verdict overlays and rollback
elision, not raw output that triage dropped. Images are not assigned text-byte
costs; this threshold measures text only.

The session TOML key `consolidation-bytes` sets the threshold. The default is
65,536 bytes (64 KiB); zero permits checking any nonempty tool-work budget.
Dialogue growth and elapsed time do not independently qualify a check.

One continuation of the session's main model both judges the opportunity and
writes the account. It reads the full projected working context with the same
model, system, tools, tool choice, thinking and effort policy as the main
request. The instruction follows a breakpoint on that shared prefix. It asks
for account text or `skip`, identifies the eligible range and retained
surroundings, and instructs the model not to use tools. A returned tool call
executes nothing and is a no-change outcome.

The check runs as a detached task, as the title ask does
(`docs/session-title.md`); the loop does not wait for it. The candidate range,
budget and projected context are read synchronously at the juncture, so the
request is built from that frozen snapshot while the trajectory continues past
it. One check is in flight per session: a juncture reached while one runs is not
checked. The task's client does not answer to the operator's interrupt, which
stops the trajectory, not a request outside it.

An account is adopted when it lands, by appending its span mark; the next main
request projects it, and a request already in flight is unaffected. Entries
appended after the snapshot stay after the range, including calls still
awaiting results. Adoption is rejected when something appended since the
snapshot changes what the range projects to: a fold, a rollback, a verdict on a
result at or before the snapshot's end, or a `consolidate` call, whose own range
was computed without this one. The tail that accumulated while the account was
written is sent uncached once after adoption, along with the account. There
is no summary approval or editor: the operator can read the adopted account and
correct it with an ordinary turn.

A skip retains the outstanding work budget. The projected context of the last
examination is tracked independently, including a durable context sequence,
so unchanged opportunities are not repeatedly checked, including after
resumption. New forward context can make old outstanding work useful to
consolidate without requiring a new tool call. After adoption, eligibility is
measured only over the newly reachable working span.

## Accounts and failure handling

The account is a note for continuation, not a chronological recap. Prefer
completed tool-heavy reads, investigation, edits, execution and verification.
Prefer leaving pure planning/design intact, without mechanically prohibiting
its consolidation. Skip when much of the eligible range remains useful
verbatim for ongoing or next work. Preserve current results, decisions and
reasons, scoped still-operative direction, unresolved threads and relevant
verification. Omit superseded diffs and resolved compile/troubleshooting
sidetracks unless they establish an enduring constraint.

Exact snippets, names and line ranges require directly visible support.
Historical observations are not necessarily current after later edits; qualify
or omit uncertain precision. The writer makes no fresh verification calls.

Explicit `skip` (case/whitespace-insensitive), fewer than 100 words, any tool
call, an upstream error or interruption, or a non-finished output such as an
output cap leaves the context unchanged. Transport retries apply as they do to
every request on the session's client; once a response is judged, the check is
not asked again at the same juncture, so permission to skip does not become
pressure to write.

`Telemetry::Consolidation` records the frozen range, context sequence, projected
tool bytes, checking/adopted/skip disposition and adopted account size. These
entries project to nothing. Usage is persisted with origin `consolidation`,
which reads the main prefix but does not become the main diagnostic comparison
base. TUI and web transcript accounts display the replacement bounds and entry
count; the control transcript names the range and account.

## The agent-called tool

`consolidate(summary)` is dispatched structurally by `src/agent/step.rs`, not
through a world-facing provider. After the normal permission cascade approves
it, the loop validates a complete new span and an account of at least 100 words,
then appends a span mark and a paired `ToolResult`. Invalid requests receive an
error result and no mark. The current consolidate call's entire assistant batch
stays outside the replacement. Called as the first action after a user
message, that message and the report it answers are retained; called mid-task,
the range runs through the last tool result and includes the instructions the
agent is working under, which the tool description tells it to carry forward. The tool call/result remain visible as resolved
bookkeeping and do not contribute to automatic eligibility.

## Projection and head folds

The projection pre-pass collects applicable marks and unions their elided
sequence ranges. `Span` removes only `from..=upto`; `Boundary { upto }` continues
to remove the head `seq <= upto`. Both elide their own mark entry and inject
their account in collected log order at the range end. A `Span` account is
introduced by a `⟢ consolidation: entries #from–#upto were replaced…` line, the
harness-note form the session prompt describes, so it does not read as an
operator turn; a `Boundary` note projects as written, since the narrator's
account carries its own heading. Rolled-back marks and marks for another
reader are ignored. Projection is deterministic; the bytes before a span's
start remain stable, and the replacement deliberately invalidates cache reuse
from that start onward.

Head folds are a distinct operation used by the digest/narrator and cold-fold
paths (`docs/summary-head.md`). The additive `Span` representation does not
reinterpret persisted `Boundary` entries or change `Audience::Triage` behavior.
Pressure-triggered digest backstops are separate from conversational checks:
an uninterrupted tool trajectory can have no eligible juncture for a long time.
