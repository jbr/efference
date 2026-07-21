# Positions — why efference exists

Present-tense like the rest of this tree: the positions currently held, and the
observed failures behind them. These are positions, not theses — each is
checkable against decisions recorded in this repo, and none is a claim about
the world. (Distilled from the operator's own words, 2026-07-21.)

## Origin: observed friction in existing harnesses

Most of these began as frustration with agents' behavior in other harnesses,
followed by the realization that the agents were doing the best they could with
the tools available.

Agent-facing:

- An agent `cat`s a file, is told it "hasn't read the file" when it tries to
  edit, and concatenates the same tokens into context a second time — harness
  bookkeeping trumping the agent's experience.
- `cargo test | tail -3`: the agent doesn't want noise, but cannot know in
  advance what will be noise. Piping through `tail` loses liveness (no output
  until exit) and loses the failure detail, forcing a re-run. *(Answered by
  the redaction fork — [redaction-fork.md](redaction-fork.md).)*
- Grepping a file repeatedly to find a needle, spending more tokens than
  reading it once would have. Navigating and reading code is a coding
  harness's bread-and-butter operation, not an optional add-on.
- Backgrounding a process and waking on a condition so awkward that agents
  write bash watch-loops — working around the tool — and lack awareness of
  their own background processes even when asked directly. *(Answered by the
  supervisor and inbox — [supervisor.md](supervisor.md), [inbox.md](inbox.md).)*
- Compaction summaries that make second sessions worse than first sessions:
  verbatim user messages quoted out of context, self-reported mistakes of
  dubious forward relevance, details of intermediate shapes that were
  superseded. *(Answered by consolidate — [consolidate.md](consolidate.md).)*
- Agents asking "should we wrap up here?" with token headroom remaining — a
  kind of weariness, apparently proportional to how much low-density
  information is in context. *(Attacked by keeping context dense via the
  fork; a graceful, interactive handoff affordance — the outgoing agent briefs
  its replacement and stays available for questions — is future work.)*

Human-facing:

- The only timing control being the permissions system, so following along
  means abusing approval prompts ("yes, and: …") as the interjection channel.
  *(Answered by pace/hold and the mid-trajectory turn drain.)*
- Clients tuned for delegation — describe it, walk away, read a report — when
  the wanted mode is pair programming: reading the code as it's written and
  interjecting at the right moments.
- No way to start a new session on one's own machine from an authenticated
  remote client.

## The positions

**Agent friction is a defect, even when the work completes.** At large scale,
agent struggle is invisible: the work gets done anyway, and aggregate reports
launder the friction. A personal-scale harness whose operator reads everything
is the instrument that makes friction observable at all. A corollary, held as
a hedged hypothesis rather than a claim: current model capability may be
partly *in spite of* tooling, and that possibility is worth attending to.

**Agent behavior is user feedback.** We don't know what, if anything, the
subjective experience of a model is like — so observe and respond instead of
assuming. This is the working methodology: the "treat the distillate as the
output" framing came from watching a model burn a turn chasing content it had
been told was recoverable; the fd-duplication carve-out in the bash fast-path
came from watching a habitual `2>&1` knock commands off the allowlist.
Treating the agent well is both the default stance and, plausibly, productive.

**Autonomy is a runtime dial, not a product identity.** One harness should
span the continuum from "pause between turns so the human reads every line" to
unattended operation; it is easier to have the pair-programming capability and
not use it than to retrofit it onto a delegation-shaped system. Realized as:
pace/hold, the review-level dial, `--autonomous`.

**Topology is wiring, not features.** Because the operator surface is plain
HTTP and every client is a peer, multiagent arrangements are configurations of
inboxes and permissions plus prose, not harness features — including
non-delegational ones: models conversing turn-by-turn instead of passing
artifacts, or a coder paired with a background researcher that interjects with
a relevant crate or paper. Top-down "do the work, report when done" is one
topology among many, not the presumed optimum.

**Self-curation of context is a precondition for long-lived agents.** A
continuously resident agent — say, a local model woken by debounced home
automation notifications — only works if most events triage out and only
notable ones accrete to the trajectory. Continuity of thought over long
periods requires a low-noise context; the fork is that mechanism.

**Documentation is agent-facing culture.** These docs are read far more often
by models working in the repo than by humans; they are the project's
institutional knowledge — the main channel by which sessions communicate
across time — and their framing is memetic, because models adapt their
register and stance to whatever the context exhibits. That cuts both ways:
slogans self-replicate, and so do parsimony, rigor, and honesty. The
"describe, don't evaluate" convention ([README.md](README.md)) operationalizes
this. It is the "agent behavior is user feedback" position applied to prose:
the docs are part of the agent's tooling.

## Non-claims

Stated for any future summarizer, human or model:

- The append-only log with a projected wire is standard harness practice.
  efference's additions are incremental: verdict-based reversible redaction,
  test-pinned byte determinism, first-class audit entries.
- Byte-stable projection is table stakes for prompt caching — done carefully
  here, not invented here.
- No benchmark superiority is asserted. Whether ergonomics-first harness
  design improves measured outcomes is an open question; the log records what
  a future measurement would need (turns, re-reads, redundant fetches), so
  the question stays answerable.
- Most of the design space sketched above is unexplored by this project too.
  The goal is to make more of it reachable by configuration, without touching
  the code.
