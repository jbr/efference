# Triage

A test run can produce hundreds of lines when the agent needs just the failure. A file
read can bring in a whole module when it needs one function. **Triage** is how efference
selects what the agent reads: after a batch of tool calls, a model reads each result,
keeps the relevant lines, and adds a one-line note about the rest. The agent receives
those excerpts and notes rather than the raw output.
<!-- src: src/agent/landing.rs::land; src/triage.rs::Triager::triage_results; src/triage/schema.rs::retain_tool; src/project/render.rs::render_hunks -->

## Give the agent the output it needs

Triage is part of the session’s normal workflow, not a mode you have to turn on. It applies
to tool results from shell commands, file reads, and MCP tools, small results included.
There’s no off switch; the limits and special cases are [listed below](#limits-and-exceptions).
<!-- src: src/agent/landing.rs::land; src/triage.rs::Triager::triage_results; src/triage/schema.rs::retain_tool; src/project/render.rs::render_hunks; absence of a disable setting in src/config/raw.rs, src/bin/efference/cli.rs -->

This lets the agent run commands without guessing which lines to cut away first. Instead
of `cargo test | tail -3`, which might hide the failure it needs, it runs `cargo test` and
says what it’s looking for. The triage model uses that intent to choose what to keep. The
aim is to spend the agent’s context on information it needs for the work, not on the volume
of output a command happens to produce.
<!-- src: docs/positions.md ("Origin"); assets/prompts/session.md (do not pre-filter); src/log/names.rs::INTENT -->

You can inspect both versions: the session log keeps the full output, subject to the
[result-size limit](#limits-and-exceptions), and the clients can show it beside the selected
lines. The agent can’t retrieve dropped lines from the log. If it needs more, it runs the
command again, often with a narrower question.
<!-- src: src/agent/batch.rs::run_calls (raw logged before triage); src/tui/block_menu.rs::menu_items (raw / triaged); web/src/ui/Inspector.tsx (raw tab); src/agent/surface.rs::tool_definitions (no retrieval tool) -->

## Choose a triage model

You can start with the default: triage uses the agent’s own model and connection. It adds
one model request per batch of tool calls, branching from the agent’s conversation. If
your provider caches prompts, that branch reuses the cached conversation. The new tool
output, instruction, and triage answer are the parts charged at full price.
<!-- src: src/triage.rs::Triager::shared, choice; src/inference/options.rs::resolve_triage; src/triage/prompt.rs::triage_messages (breakpoint before the tail) -->

A model with no prompt cache pays for the whole conversation on every triage. Check what
your model supports before pairing it with a long session.
<!-- src: src/api/provider/capabilities.rs (PromptCachePolicy::None) -->

To use a different model for this work, choose a **triage model** in the new-session form.
That can be a cheaper model, but it has its own cache rather than sharing the agent’s.
It receives every tool result and a text record of the session: recent steps in full, and
one line per older step. Choose its connection with the same care as the main model’s;
your project’s content goes there too.
<!-- src: src/tui/session_form/draft.rs::TriageDraft; web/src/ui/NewSessionSettings.tsx; src/triage.rs::Triager::own; src/triage/text_view.rs::history; src/digest.rs -->

The triage model also writes each session's title.
<!-- src: src/triage/title.rs; src/agent/title.rs -->

## See what the agent was shown

Look for a label such as “triaged, kept 3 of 412 lines” on a result. The note beneath it
explains what the rest contained; open the full result to compare. If something important
was left out, tell the agent so it can follow up. There’s no control for changing a triage
decision after the fact.
<!-- src: src/tui/projection.rs::result_entry, selection_note; web/src/render/parse.ts::wireLabel; src/log.rs::result_verdicts (first verdict wins) -->

If triage can't reach its model, or can't produce a usable answer after several tries, the
session stops and says the results are held. The agent is never handed the output untriaged,
or an error in its place. Continuing tries triage again; interrupting ends the turn.
<!-- src: src/agent/triage_hold.rs::hold; src/triage/request.rs::TRIAGE_ATTEMPTS, TRIAGE_RESTARTS -->

## Triage details

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
<!-- src: src/log/names.rs::INTENT; src/tools.rs::with_intent; src/agent/surface.rs::tool_definitions; src/project/render.rs::INTENT_FEEDBACK_LEAD -->

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

### Requests you may notice between turns

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

### Triage, permissions, and summaries

- **The permission spotter** judges a tool call *before* it runs, when the project's
  settings don't already allow it ([Permissions](permissions.md)). Triage reads results
  *after*.
- **Consolidation**: the agent replaces a stretch of its own finished work with a summary
  it writes, from the work as it saw it. It happens on its own once enough tool work has
  built up, or when the agent chooses.
- **Compaction**: efference summarizes older parts of a session from its record, at your
  request; it never happens on its own.
<!-- src: src/spotter.rs; src/agent/consolidation.rs::ConsolidationCheck::check, ask; src/consolidate.rs; src/daemon/compact.rs::NotCompactable; web/src/ui/CompactControl.tsx -->
