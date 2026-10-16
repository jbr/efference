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
- **The boot `Preamble`**, whose `identity` line and `body` the resumed session reuses
  byte for byte (the line is replaced only by a main-model switch, below) and whose
  `context` re-renders the triage prompt, so no context command reruns. A log
  without one can't be resumed.
- **Result capture.** A call's result is appended `Pending` as soon as the call returns,
  and its decision lands later as a `ResultVerdict`
  ([triage.md](triage.md), "Capture, then verdict").
- **Arrived notifications.** A notification without a slot is an `Ambient` entry from its
  arrival, carrying its must-wake flag, and each `Triage` records `shown_through`, the
  last arrival its rider showed. Together they say which notifications are still unshown
  ([inbox.md](inbox.md)).
- **`CallStarted`**, a non-projecting entry appended after the permission cascade clears
  a call and before the tool runs (never for `ask`). It records which calls may have
  touched the world: a call with it and no result is the one case that rolls back.

## The loader

`boot::resume` (`src/boot/resume.rs`) rebuilds a session from its directory, sharing
the boot steps a creation uses (`src/boot/{scaffold,env,model,parts}.rs`) and replacing
the ones that mint:

- **Inference** comes from the header metadata (spotter and triage from the latest
  preamble's `roles` when it records any, then any switch in the request), mapped field for field onto
  `SessionOptions`. With `metadata: null`, the main model comes from the `Preamble`, and
  the spotter and triage models from the host's defaults. The options it inferred are
  returned to the host, so a `hand_off` from a resumed session passes on the model it is
  running.
- **Config** is the session's own `config.toml`, the live policy, not the template.
- **Environment and scratch** are established fresh; neither is persisted.
- **MCP servers** are mounted from config, as at creation. The `tools[]` that results is
  compared with the last one the log recorded (`Telemetry::Tools`, recorded at boot): the
  resume proceeds on a difference, and the restart notice names the tools added, removed
  or changed. The new array is then recorded, so the next resume compares with it; a
  resume with an equal array records nothing, and a log that never recorded one has
  nothing to compare with. The cache is cold on a resume with a difference, since the
  prefix moved.
- **The log** is opened with `Log::open` (writer lock, torn-tail repair), as the last
  fallible step. Everything before it reads without writing, so a resume that fails
  leaves the directory byte-identical, and rollback removes only the scratch directory
  this attempt made.
- **State**: the turn counter continues from the last entry's turn; the title is the
  last `Title`; `p#` handles are minted above the highest one the log mentions.

**Per-call classification** (`src/recovery.rs`, read by the loader and by the agent loop).
Each call of the log's last live completion is in one of four states:

- **Decided**: a `ToolResult` that is not `Pending`, or is `Pending` with a `ResultVerdict`.
- **Captured, undecided**: a `Pending` result with no verdict. Triage runs on the stored
  result; the call does not run again.
- **Never started**: no result and no `CallStarted` (a call held at an approval gate, or
  an `ask` with no reply). It goes through the permission cascade again, judged against
  current policy rather than the approval context the log recorded.
- **Started, no result**: `CallStarted` and no result. The call may have touched the
  world and its outcome is unknown.

A log with no `CallStarted` entry anywhere predates the marker. There a resultless call
counts as never started when a `Spot` for that call id has stance `gate` (or `watch`,
which resolves to the same human gate), and as started otherwise. A call approved at the
gate and then lost looks the same as one still held, so this errs toward rerunning it
through the cascade.

**The resume point.** A log ends cleanly when its last completion has no calls or every
call is decided; it resumes after its last entry. A completion with undecided calls and no
started call is a **pending batch**: the session resumes at it, with nothing rolled back,
and the `Resumed` entry records the calls still to come back in `pending_calls`. A
completion with any started call rolls back: the resume continues from just before that
completion's assistant entry, `resumes_after` is the seq of the last entry before it, and
the same check runs on the log up to there, walking back until it reaches one of the first
two. Nothing in the skipped batch is kept, rerun or re-triaged, and the entries stay on
disk. A log with nothing before the unfinished completion can't be resumed. The skipped
completion was only ever in triage requests, so the main wire's cached prefix does not
change. Ownership is checked twice: `Host::adopt_with` refuses a second hosting of an id,
and `Log::open`'s file lock refuses a second writer, in-process included.

The loader appends one **`Resumed`** entry, which projects a restart notice:
how long the session sat, which background processes are no longer monitored (handles
with a `Spawned` event and no `Exited`, over the whole log, skipped entries included),
when the tools differ from the recorded ones (by name, as `added`, `removed`, `changed`),
when it resumed at a pending batch, that it stopped with N tool calls pending and the
operator's next action brings them back, and, when entries were skipped, that the session
did not end cleanly, that N entries after seq S were not restored, and that they may
include tool calls that ran. It gives the agent no instructions. The notice stands in for
everything a restart may have changed.

**Skipped entries.** `Resumed.resumes_after` is present only when entries were skipped;
projection (`project_semantic`, which both wire dialects, the triage and title asks and
`dev wire` encode from) drops every entry after it and before the `Resumed` entry, and a
`Summary` entry inside the range replaces nothing. A log may hold several `Resumed`
entries, each with its own range; a completion inside an earlier range is not the
"last completion" at the next resume, so resuming twice does not roll back twice. The
rest of what the log carries is read whole, so numbering stays monotonic: the turn
counter continues from the log's last entry, `p#` handles are minted above every
mention, the title is the latest `Title`, and grants, probes and triage retention fold
entries that happened whether or not they are on the wire. The TUI transcript, the
control transcript and the web client still list skipped entries, since they are on disk;
only the notice marks the range.

**A pending batch on the wire.** While the batch is unanswered, projection yields what a
live gate does: the completion's `tool_use` blocks with their results absent, and the
notice not yet projected. A user turn leads with the `tool_result` blocks that answer a
`tool_use`, so a notice naming `pending_calls` projects into the user turn right after the
last of those results, wherever the `Resumed` entry sits in the log; if the batch never
completes, it is never projected. A `Resumed` entry with no `pending_calls` projects at its
own position as before.

## Nothing runs before the operator's first action

A resumed session starts no trajectory on its own and sends no model request of its own,
whether the cache is believed warm or cold, and whether the resume was by hand or at host
start. The only thing that runs is the keepalive for a cache believed warm
([transport.md](transport.md)). The idle loop in `agent::run` waits on a user turn, an
inbox wake or a continue. A notification that arrived without a slot is a log entry, so
the loader restores the unshown ones with the rest of the log (`inbox::unshown`,
[inbox.md](inbox.md)): nothing is dropped as stale, and the next triage offers them all,
whatever their age. The idle wake does not count them: it looks only at a notification
that arrived after the session was built (a seq at or past the log's length when the
session was constructed) and at the in-memory slot queue, which a resume starts empty. So
no idle triage runs over what was restored, no title ask follows it, and no status reaches
a subscriber (a status is posted when a trajectory ends into idle, and none has run). A
notification arriving after the resume wakes the idle loop as it would any session, subject
to the cold hold, and the triage it starts offers the restored ones with it; a restored
must-wake notification is pinned at its first show as any is. What gets the agent going
again is an interaction: a turn, an empty turn, a continue. A slot notification was only
ever in memory and is lost, as are subscriptions, which are host state; keepalive waits on
clocks only a real request sets. The kickoff fires only on a log with no `User` entry,
which a resume reaches only if the host stopped between submitting the kickoff and
appending it.

**The first action replays a pending batch.** At each idle wake the loop derives the
pending batch from the log (`recovery::pending_batch`), with no flag to drift. A
continue or a submitted turn then enters the trajectory without a model request: the
batch's never-started calls go through the permission cascade in call order under the
completion's turn (a gate reappears, held for the operator and judged against current
policy), the captured-undecided results are rebuilt as steps from the log's masked bytes,
and one triage covers them all, as at the end of any batch. The next pass then projects and
requests as usual. The spotter and triage are model requests, so the gate
reappears on this first action, not when the session opens. `POST /loop/continue` accepts
a pending batch even though the wire ends in a `tool_use`, since the replay answers it.
A turn submitted first is held and delivered after the batch, as one submitted
during a live gate is; a `User` entry ahead of the unanswered `tool_use` blocks would
break the wire. While the batch is pending, an inbox wake does not run the idle triage
(its triage would project the unanswered wire): the notification waits and rides the batch's
triage.

## Archived and interrupted

Two non-projecting entries record the lifecycle. `Resumed` is appended on a reopen;
`Archived` is appended when a session is archived on purpose: `DELETE /sessions/:id`,
`PATCH {"state": "archived"}`, and a child that ends with its creator. It is the last
append of the archive, after the worktree outcome, under the turn of the log's last
entry. A host shutdown stops its sessions without appending it, and so does a session
whose loop ends on its own.

The later of the two is the session's lifecycle. A log whose last lifecycle entry is not
`Archived` (including one with neither) was open when its host stopped, and the dormant
listing marks its row `interrupted` (`src/inventory.rs`).

## Sessions that never had a turn

A log is *empty* when it holds no `User` and no `Assistant` entry: the preamble,
telemetry, the `Ambient` and `Triage` entries of an idle triage, `Worktree` and
`Archived` are the harness's own record, so a session with only those has lost nothing
the operator wrote or the agent did (`src/discard.rs`). Such a session is discarded, not
archived:

- **At archive.** Each archive path (`DELETE`, `PATCH {"state": "archived"}`, a child
  ending with its creator) removes the session directory and releases the id's row in
  the `session_ids` register ([session-title.md](session-title.md)), so the id can be
  minted again, and appends no `Archived`. The response says `"state": "discarded"`.
  Removal refuses anything that is not a real directory directly under the project's
  `.efference`; if it fails, the session is archived as usual. A host shutdown discards
  nothing.
- **Worktree.** A worktree the session made is retired first
  ([worktrees.md](worktrees.md)). A clean one is removed and the discard goes on, the
  response carrying `worktree: {"removed": true, ...}`. One that is kept or whose removal
  failed makes the session an ordinary archive.
- **At host start.** The background pass that registers ids
  ([session-title.md](session-title.md)) first sweeps the remembered roots for empty
  logs and removes them, whether or not they carry an `Archived` entry: the leftovers of
  a host that stopped between a creation and its first turn. A log last written at or
  after the host started is skipped, since a session this host is already running may
  own it. A log that does not parse is logged and left.

## The session as a resource

`PATCH /sessions/:id` with `{"state": "active"}` revives a session
([control-api.md](control-api.md); `src/daemon/host/resume.rs`). `state` is the
lifecycle, `active` or `archived`, separate from the `status` a listing reports for what
a live session is doing. `DELETE /sessions/:id` is the same transition as
`{"state": "archived"}`. The body carries `cwd`, since the path alone can't locate a
dormant session's directory, plus the inputs a revival needs (`env`, `reauth`),
and no option for effort or temperature: a resume reads them from the header. It may carry
a **model switch**, below. The header also holds
`detached`, which decides whether the resumed session ends with its creator when
the header's `created_by` names a session this host still runs; a creator that is
gone is dropped.

**Switching models.** The body may carry `connection` and `model` (the main model),
`spotter_connection` and `spotter_model`, and `triage`, in the shapes creation takes. A
role left out keeps its recorded selection; naming either field of a model's pair
replaces that selection as a whole. The selections resolve through the registry creation
uses (`InferenceRegistry::resolve_options`, then `prepare`), so a selection creation
would refuse refuses the resume with the same reason, and the directory is left as
found. `QUERY /sessions` runs that resolution on the same fields, so a client previews a
switch's issues before sending it; the preview takes the full selection, since it has
no session to read the unnamed ones from.

When a switch changes what the session runs, the resume appends a `Preamble` that
repeats the frozen one with the new `roles` ([log-and-projection.md](log-and-projection.md)),
after the `Resumed` entry; a switch that names what is already recorded appends nothing.
The main model resolves from the latest preamble's `inference`, and the spotter and
triage from its `roles`, each else from the header, so a later resume without a switch
keeps the switched values. The projection is untouched.

*Spotter and triage.* The `system` text and the main cache are untouched. Switching
triage starts a new triage lineage: the triager is built at resume and holds no state
from the old one (the triage keepalive's last request and the primer are in-memory, set
by the first triage after the resume), so the first triage reads cold and the primer or
the keepalive take over from there. Moving between shared and separate triage needs
nothing more.

*Main model.* A switch keeps the session's id and log; a new identity is the hand-off's.
The appended `Preamble` carries the new model's identity line (`You are <model>.`) over
the old `body` byte for byte, so no `[context]` command reruns and no file is re-read,
and the next request's `system` differs from the last one's in that line alone. The
model must speak the wire dialect the log's assistant turns are recorded in; one that
does not is refused naming both dialects (`check_dialect`), and so is a log whose
prompt has no identity line to replace (written before the line existed). The session
starts cold: usage-record requests from before the switch cached the old model's
prefix, so they seed no cache-touch clock (the keepalive has nothing to ping) and the
`cold` topic reports the session cold from the moment of the resume
(`Session::mark_cache_lost`) until a request on the new prefix is made, which is a
person's action ([control-api.md](control-api.md); a compaction before it also ends the
belief). Effort and temperature are the header's, so a model that refuses the recorded
effort refuses the resume.

The PATCH answers once the session is reserved as **starting**
([control-api.md](control-api.md), "A starting session"): the environment and
`boot::resume` run after the answer, the session is listed as `starting` meanwhile, and a
turn sent then is the first it runs. A resume that fails there leaves the session
dormant: the starting entry leaves the host's map, its stream ends with the reason, and
the dormant listing carries the reason as the row's `resume_failure` until a resume starts
the session. That is not a `problem`: the session reads fine and stays resumable, and a
retry is the same PATCH. Archiving a session while it is starting cancels the resume and
ends its log in `Archived` unless that is already its last lifecycle entry, so it reads as
archived rather than interrupted.

**The resume at boot.** After the host-start pass that sweeps empty logs and registers
ids, a background task (`Host::resume_open_sessions`, `src/daemon/host/boot_resume.rs`)
reads the remembered roots and resumes every session whose row is `interrupted` and has no
`problem`, through the same `Host::resume` the PATCH reaches: each is reserved as
`starting` and prepared in the background while the host serves, with
`refuse_authorization` set so a server that asks for authorization fails that session's
resume instead of opening a browser for each. At most four sessions prepare at once
(`BOOT_PREPARING`); the next starts when one has started or failed. A session that fails
stays dormant with its `resume_failure`, and nothing in the pass stops the host. The pass
runs creators before the children that end with them: a session whose header's
`created_by` names another session in the pass waits until that one is hosted, not merely
starting, because `recorded_lineage` keeps a creator only while it is hosted. A creator
that failed leaves its child standing on its own, as one whose creator was archived does,
and a creator chain that loops back on itself is broken. `detached` is read from the
header, so a non-detached child ends with its creator again. Session ids are unique
across roots (the register), so a session is resumed from one root only. The supervisor
pointer and succession are host state and start again from the creator
(`Hosted.supervisor`), as after any resume.

The PATCH is idempotent. On a live or starting session it answers `200` with
`resumed: false`; the revival itself answers `resumed: true`. The same id live under a different root is `409`
(`state_mismatch`), and so is a dormant directory whose id the `session_ids` register holds
under another root ([session-title.md](session-title.md)); a root the register has not seen
claims the id.

`GET /sessions?state=archived` lists the dormant sessions under a root, or under every
remembered root, from a scan of each log's header, first and last entries and last title
(`src/inventory.rs`; [log-and-projection.md](log-and-projection.md), "Inventory of dormant
sessions"). A directory that can't be resumed is listed with a `problem`; a session
whose last resume on this host failed is listed with its `resume_failure`.
`GET /sessions/:id` answers one session's row, the hosted listing's for a hosted session
and this listing's for a dormant one, so a dormant session is addressable by id.

## Clients

- **Bare `efference`**: the picker's "resume an archived session…" entry opens a list of
  this directory's resumable sessions, filtered by title or id as you type. A pick
  revives the session and attaches; with no host running, it resumes into a terminal
  island ([operating.md](operating.md)).
- **The TUI's session switcher**: `r` opens the same page (`src/tui/resume_list.rs`) as a
  modal, and a pick switches to the revived session while it starts ([tui.md](tui.md)).
- **Web client**: dormant sessions surface on the overview across four continuum bands:
  active cards, recently archived cards (< 1h, capped), compact listed rows (< 1w),
  and a full-screen filterable picker (`?resume=picker&cwd=...`). A top-bar `Resume…`
  action opens workspace selection (`?resume=location`) before landing on the picker
  ([web-client.md](web-client.md)).
- **Scripts** send the PATCH over HTTP. There is no `resume` control tool: the control
  tool set is also mounted into every session's `tools[]`, and reviving a session is not
  a step the main agent needs.
- An archived `--name` is refused, as before.
