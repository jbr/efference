# Triage

## What you need to know

The agent does not read raw tool output. After it runs a batch of tool calls — shell
commands, file reads, MCP tools — a model reads the full output of each and keeps the lines
that matter, with a one-line note about the rest. The agent sees those kept lines and the
note. This is **triage**. It applies to every tool result, small ones included, and it has
no off switch.
<!-- src: src/agent/landing.rs::land; src/triage.rs::Triager::triage_results; src/triage/schema.rs::retain_tool; src/project/render.rs::render_hunks; absence of a disable setting in src/config/raw.rs, src/bin/efference/cli.rs -->

It exists because an agent can't know in advance which part of a command's output it will
need. Without triage, an agent either reads everything, filling its context with noise, or
cuts the output down itself — `cargo test | tail -3` — and loses the failure it needed. With
triage, the agent runs commands plainly, and the model choosing what to keep can see the
whole output and what the agent said it was looking for.
<!-- src: docs/positions.md ("Origin"); assets/prompts/session.md (do not pre-filter); src/tools.rs::INTENT -->

The full output is kept in the session's log, and the clients can show it beside the kept
lines. The agent itself can't ask for dropped lines back; if it needs them, it runs the
command again, often more narrowly.
<!-- src: src/agent/batch.rs::run_calls (raw logged before triage); src/tui/block_menu.rs::menu_items (raw / triaged); web/src/ui/Inspector.tsx (raw tab); src/agent/surface.rs::tool_definitions (no retrieval tool) -->

### What it costs

Triage is one extra model request per batch of tool calls. By default it runs on the
agent's own model and connection, as a branch of the agent's conversation. With a provider
that caches prompts, that branch reuses the cached conversation, so the new part — the tool
output and the instruction — is what costs full price, plus the triage's own answer.
<!-- src: src/triage.rs::Triager::shared, choice; src/inference/options.rs::resolve_triage; src/triage/prompt.rs::triage_messages (breakpoint before the tail) -->

A model with no prompt cache pays for the whole conversation on every triage. Check what
your model supports before pairing it with a long session.
<!-- src: src/api/provider/capabilities.rs (PromptCachePolicy::None) -->

You can give one session a different, cheaper triage model: the "triage model" choice in
the new-session form. That model has its own
cache, and it receives every tool result plus a text record of the session: recent steps in
full and one line per older step. Its connection is one more place your project's content
goes.
<!-- src: src/tui/session_form/draft.rs::TriageDraft; web/src/ui/NewSessionSettings.tsx; src/triage.rs::Triager::own; src/triage/text_view.rs::history; src/digest.rs -->

The triage model also writes each session's title.
<!-- src: src/triage/title.rs; src/agent/title.rs -->

### Checking its work

A result the agent got only part of is labelled with how much it kept, such as "triaged,
kept 3 of 412 lines". The note under it says what the rest was. Open the result whole to
compare. If triage dropped something the agent needed, tell the agent, as you would about
anything else; there is no control that changes a verdict after the fact.
<!-- src: src/tui/projection.rs::result_entry, selection_note; web/src/render/parse.ts::wireLabel; src/log.rs::result_verdicts (first verdict wins) -->

If triage can't reach its model, or can't produce a usable answer after several tries, the
session stops and says the results are held. The agent is never handed the output untriaged,
or an error in its place. Continuing tries triage again; interrupting ends the turn.
<!-- src: src/agent/triage_hold.rs::hold; src/triage/request.rs::TRIAGE_ATTEMPTS, TRIAGE_RESTARTS -->

## How this works, precisely

### One request per batch

A model reply can make several tool calls. efference runs them all, records each full
result in the log, and then sends one triage request covering the batch. The request is the
agent's conversation so far, followed by each result with numbered lines and an
instruction to answer, for each result, with the line ranges to keep (or all, or none) and a
note. The request is a branch: neither it nor the full output enters the agent's
conversation, only the verdict does.
<!-- src: src/agent/batch.rs::run_calls; src/agent/landing.rs::land; src/triage/prompt.rs::result_view; src/triage/schema.rs::retain_tool; src/triage/parse.rs::parse_retain -->

The project's tools, and efference's file and shell tools, carry an `intent` field: a
sentence saying what the agent wants from the call. The triage model reads it and the note
answers it. When the intent gave it nothing to decide against, it can say so to the agent.
<!-- src: src/tools.rs::INTENT, with_intent; src/agent/surface.rs::tool_definitions; src/project/render.rs::INTENT_FEEDBACK_LEAD -->

The same request also sees pending notifications and the output of the session's
background processes, and decides which deserve the agent's attention.
<!-- src: src/triage/schema.rs (inbox, background fields); src/agent/landing.rs::land (open_rider) -->

### What the agent sees

The kept lines appear verbatim, each range headed by its line numbers, followed by the
total line count when the end was cut, then the note. When every line is kept the agent
sees the output exactly as it was. Triage judges only text: images in a result reach the
agent with whatever text is kept. Triage reads the output after secret values are masked
([Credentials](credentials.md)).
<!-- src: src/project/render.rs::render_hunks, NOTHING_KEPT; src/triage/prompt.rs::result_view (images not shown to triage); src/project/semantic.rs::render_result; src/agent/batch.rs::run_calls (reads the masked entry back) -->

### Limits and exceptions

- Triage reads the first 2,000 lines of a result. The agent is told how many lines past that
  were not read.
- A result over 256 KiB is not kept: efference replaces it, in the log too, with an error
  carrying its first kilobyte and telling the agent to narrow the call.
- The terminal interface shows the first 16 KiB of a result; the web client can load the
  rest.
- Some results reach the agent without triage: a reply to a question the agent asked, and a
  file-editing tool's refusal that carries a retry token. A call that never ran — refused,
  or an unknown tool — has nothing to triage. A call you interrupt while it runs is
  recorded, and the agent is told it was interrupted instead of seeing its output.
- A separate triage model's answers are capped at 16,384 tokens.
<!-- src: src/project.rs::TRIAGE_LINE_CAP; src/agent/results.rs::cap_tool_result; src/agent.rs::MAX_TOOL_RESULT_BYTES; src/daemon/entry_view.rs::MAX_INLINE_RESULT_BYTES; web/src/ui/ToolBlocks.tsx (fetchWhole); src/triage/handles.rs::bypassed, SHAPES; src/agent/step.rs::run_call (Decision::Withheld); src/triage.rs::TRIAGE_MAX_OUTPUT -->
<!-- TODO: the 256 KiB replacement means "raw is never overwritten" holds only below the cap; raised with jbr 2026-10-19. -->

### Other requests

Besides one triage per batch, a session makes a few small requests of its own:

- **Cache keepalive.** Providers keep a conversation's cache for a few minutes. While a
  session is idle, efference sends a small request just before that cache would expire, for
  up to 30 minutes after the last real turn, and skips them when the last one cost more
  than it saved. A separate triage model's cache is kept warm the same way.
  `[transport] keepalive-interval` in `efference.toml` sets the interval in seconds; `0`
  turns it off. The clients show these as "warming the cache".
- **Notifications while idle.** When the session is idle and a notification arrives, a
  small triage on the agent's own model decides whether to wake it.
- **Titles**, written by the triage model as a session goes.
<!-- src: src/daemon/keepalive.rs::keepalive_interval, skip_reason, CAP, KeepaliveSpec::triage; src/config/raw.rs (keepalive-interval); src/tui/operations.rs::request_text; web/src/ui/operations.ts::requestText; src/agent/triage.rs::run_idle_triage; src/triage.rs::Triager::idle_envelope; src/agent/title.rs -->

When a session's cache is believed to have expired, its next request — a turn or a
triage — waits for you to continue it or compact it, rather than paying to re-read the
whole conversation unasked.
<!-- src: src/daemon/cold.rs::await_warm_or_person -->

### Not to be confused with

- **The permission spotter** judges a tool call *before* it runs, when the project's
  settings don't already allow it ([Permissions](permissions.md)). Triage reads results
  *after*.
- **Consolidation**: the agent replaces a stretch of its own finished work with a summary
  it writes, from the work as it saw it. It happens on its own once enough tool work has
  built up, or when the agent chooses.
- **Compaction**: efference summarizes older parts of a session from its record, at your
  request; it never happens on its own.
<!-- src: src/spotter.rs; src/agent/consolidation.rs::ConsolidationCheck::check, ask; src/consolidate.rs; src/daemon/compact.rs::NotCompactable; web/src/ui/CompactControl.tsx -->
