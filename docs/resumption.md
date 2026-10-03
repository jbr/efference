# Session resumption

A persisted session can be reopened under its existing id, directory and sequence space.
A resumed session is the same session, not a successor: its `config.toml`, attachments,
title and log addresses carry over, and new entries continue the old `seq` and turn
numbering. Opening it does not advance the trajectory: no model request, tool call or
`[context]` command runs until the operator acts.

## What a log must record

Resumption reads only the log ([log-and-projection.md](log-and-projection.md)):

- **The v1 header** on line 1 carries the session's id, cwd, model selections and
  generation parameters (`SessionMetadata`), never credentials. A log migrated from
  before the header has `metadata: null`.
- **The boot `Preamble`**, whose `system` text the resumed session reuses byte for byte
  and whose `context` re-renders the triage prompt, so no context command reruns. A log
  without one can't be resumed.
- **Result capture.** A call's result is appended `Pending` as soon as the call returns,
  and its decision lands later as a `ResultVerdict`
  ([redaction-fork.md](redaction-fork.md), "Capture, then verdict").
- **`CallStarted`**, a non-projecting entry appended after the permission cascade clears
  a call and before the tool runs (never for `ask`). The rollback rule does not read it;
  it records which calls may have touched the world.

## The loader

`boot::resume` (`src/boot/resume.rs`) rebuilds a session from its directory, sharing
the boot phases a creation uses (`src/boot/{scaffold,env,model,parts}.rs`) and replacing
the ones that mint:

- **Inference** comes from the header metadata, mapped field for field onto
  `SessionOptions`. With `metadata: null`, the main model comes from the `Preamble`, and
  the spotter and triage models from the host's defaults. The options it inferred are
  returned to the host, so a `hand_off` from a resumed session passes on the model it is
  running.
- **Config** is the session's own `config.toml`, the live policy, not the template.
- **Environment and scratch** are established fresh; neither is persisted.
- **MCP servers** are mounted from config, as at creation.
- **The log** is opened with `Log::open` (writer lock, torn-tail repair), as the last
  fallible step. Everything before it reads without writing, so a resume that fails
  leaves the directory byte-identical, and rollback removes only the scratch directory
  this attempt made.
- **State**: the turn counter continues from the last entry's turn; the title is the
  last `Title`; `p#` handles are minted above the highest one the log mentions.

**The rollback rule.** A log ends cleanly when its last completion has no calls, or every
call has a result and every result a verdict; it resumes after its last entry. Otherwise
the resume continues from just before that completion's assistant entry: `resumes_after`
is the seq of the last entry before it, and the same check runs on the log up to there,
walking back until it ends cleanly. Nothing in the skipped batch is kept, rerun or
re-triaged, and the entries stay on disk. A log with nothing before the unfinished
completion can't be resumed. The skipped completion was only ever in triage requests,
so the main wire's cached prefix does not change. Ownership is checked twice: `Host::adopt_with` refuses a second hosting of an id,
and `Log::open`'s file lock refuses a second writer, in-process included.

The loader appends one **`Resumed`** entry, which projects a one-line restart notice:
how long the session sat, which background processes are no longer supervised (handles
with a `Spawned` event and no `Exited`, over the whole log, skipped entries included),
and, when entries were skipped, that the session did not end cleanly, that N entries
after seq S were not restored, and that they may include tool calls that ran. It gives
the agent no instructions. The notice stands in for everything a restart may have
changed.

**Skipped entries.** `Resumed.resumes_after` is present only when entries were skipped;
projection (`project_semantic`, which both wire dialects, the triage and title peeks and
`dev wire` encode from) drops every entry after it and before the `Resumed` entry, and a
`Boundary` mark inside the range folds nothing. A log may hold several `Resumed`
entries, each with its own range; a completion inside an earlier range is not the
"last completion" at the next resume, so resuming twice does not roll back twice. The
rest of what the log carries is read whole, so numbering stays monotonic: the turn
counter continues from the log's last entry, `p#` handles are minted above every
mention, the title is the latest `Title`, and grants, probes and triage retention fold
entries that happened whether or not they are on the wire. The TUI transcript, the
control transcript and the web client still list skipped entries, since they are on disk;
only the notice marks the range.

## Nothing runs before the operator's first action

A resumed session sends no model request on its own, so it cannot pay a cold cache read
the operator didn't choose. The idle loop in `agent::run` waits on a user turn, an inbox
wake or a continue. The inbox starts empty because pending notifications are not on
disk, subscriptions are host state and are not restored, and keepalive waits on clocks
only a real request sets. The kickoff fires only on a log with no `User` entry, which a
resume reaches only if the host stopped between submitting the kickoff and appending it.
A notification that arrives later wakes a standalone triage, as it would on a live
session left idle.

## The session as a resource

`PATCH /sessions/:id` with `{"state": "active"}` revives a session
([control-api.md](control-api.md); `src/daemon/host/resume.rs`). `state` is the
lifecycle, `active` or `archived`, separate from the `status` a listing reports for what
a live session is doing. `DELETE /sessions/:id` is the same transition as
`{"state": "archived"}`. The body carries `cwd`, since the path alone can't locate a
dormant session's directory, plus the inputs a revival needs (`env`, `reauth`,
`detached`), and no model options: a resume reads them from the header.

The PATCH is idempotent. On a live session it answers `200` with `resumed: false`; the
revival itself answers `resumed: true`. The same id live under a different root is `409`
(`state_mismatch`).

`GET /sessions?state=archived` lists the dormant sessions under a root, or under every
remembered root, from a scan of each log's header, first and last entries and last title
(`src/inventory.rs`; [log-and-projection.md](log-and-projection.md), "Inventory of dormant
sessions"). A directory that can't be resumed is listed with a `problem`.

## Clients

- **Bare `efference`**: the picker's "resume an archived session…" entry opens a list of
  this directory's resumable sessions, filtered by title or id as you type. A pick
  revives the session and attaches; with no host running, it resumes into a terminal
  island ([operating.md](operating.md)).
- **The TUI's session switcher**: `r` opens the same page (`src/tui/resume_list.rs`) as a
  modal, and a pick switches to the revived session ([tui.md](tui.md)).
- **Scripts** send the PATCH over HTTP. There is no `resume` control tool: the control
  tool set is also mounted into every session's `tools[]`, and reviving a session is not
  a step the main agent needs.
- An archived `--name` is refused, as before.
