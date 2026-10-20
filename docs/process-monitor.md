# Background processes — the process monitor

## The poll-loop antipattern and the fix

The poll-loop antipattern ("check, spin, check") emerges when the tool vocabulary
only offers "check," forcing the model to be its own scheduler. The fix is
**wake-on-notification**: the model never blocks waiting for a process. It ends its
turn, goes idle, and is woken when something worth its attention happens — an exit,
a matched pattern.

A process-specific blocking `wait(until=…)` makes the agent inert to a *different*
process failing while it waits. "Wait for exit" is the idle agent being woken by the
exit notification (the idle-triggered triage, below). "Wait for a pattern" is
the non-blocking `watch_process(handle, pattern)` — the drain task matches output and
fires a notification (see [watches](#watches), below).

`wait()` instead waits for the next inbox notification of any origin, then returns the
pending notifications as its tool result. It can therefore return for an exit, a watch
hit, an operator nudge, or an inter-agent message rather than one selected process.
It refuses when nothing can wake the session — no pending notification, no running
background process, and no session it is subscribed to ([tools.md](tools.md)) — so it
does not leave a turn open indefinitely.

## Process identity — one namespace, background as a property

Every tool-spawned process gets a **spawn-minted ordinal** — one durable id that
survives every property change (backgrounding today; reattach if it ever
exists). Foreground and background share the one namespace: the loop registers
a foreground bash command the moment `Bash::run` spawns it (the `on_spawn`
hook), feeds its output chunks into the same capture as it drains, and either
removes the entry when the call completes (the paired `tool_result` is the
canonical output — retaining every finished command would grow the registry
with the session) or flips it to background on detach. So a process is
addressable — and listed on the `processes` topic — from spawn, and the handoff
is a *property change* on a resource that already exists, never the appearance
of a new one.

The id has two faces:

- **In prose** — tool results, the triage, `list_processes`, notification sources —
  it renders as `p#<n>` (`process_monitor::handle`). The prefix distinguishes it from an
  OS pid where the id floats free of context; `parse_handle` is the model-boundary
  inverse, and an unparseable handle answers like an unknown process.
- **Machine-facing** — the `processes` state topic, `process_output` frames, and
  the `/processes` routes ([control-api](control-api.md)) — it is the bare
  integer. The route path is already the namespace there, and `#` is the URL
  fragment delimiter, so the rendered form must never reach a path.

## The call↔process mapping

A process also carries the **`tool_use` id of the call that spawned it**
(`ProcessState.call_id`, on `GET /processes` and the `processes` topic). It is
stamped at the mint, alongside the ordinal, so it survives the
foreground→background flip like every other property: the entry that says
"backgrounded as p#3" is the same entry a client resolves the process from after
the flip.

The mapping is read in both directions, and each direction answers a question the
other cannot:

- **call → process.** A backgrounded command's `ToolResult` is the *handoff*
  result, not the command's output ("Foreground → background handoff", below), so a
  client holding the transcript entry has nothing to show for what the command
  actually printed. With the mapping it reads `GET /processes/:id/output` — which
  works after exit too, since the capture outlives it — and renders the `lost`
  count with it, because a capture that hit the retention cap is genuinely
  incomplete.
- **process → call.** A client showing a live process row (the TUI session drawer's
  Processes, the web tasks pane) can jump to the transcript entry the process came
  from, rather than leaving the operator to find it by reading commands.

`call_id` is optional in the type because the registry does not require a call to
register a process; every process the harness spawns today has one.

The **OS pid is an attribute, not the identifier**: the `processes` topic carries
it so a client or the agent can reach the process directly (signals, `lsof`) when
the process monitor doesn't model what it needs. It cannot be the identity because pids
are recycled and the log is durable — a logged pid stops resolving after reuse,
resumption, or a reboot.

The mint counter **seeds from the log at boot** (`max_handle_mention` over each
serialized entry). Today that scan always runs over an empty log — creation mints
a fresh session dir and refuses an existing one ([control-channel](control-channel.md)),
so no boot ever starts from a prior session's entries; the seeding is what would
keep a resumed daemon from re-minting an id the durable record already refers to.
Handles land in prose (tool results, notification sources), not in a typed field,
so the scan is textual and deliberately over-matches — a false hit skips an
ordinal, which costs nothing, while an under-match would collide.

## Process-group ownership

Every spawn is placed in a fresh process group (`process_group(0)` — the child pid
equals its pgid). On teardown the daemon calls `killpg` with that pgid, taking down
the command and any grandchildren it spawned. The `kill_group` helper in `bash.rs`
is shared between the foreground abort path and the process monitor's kill/teardown path.
The process monitor registry exposes `running_pgids()` — the set of all still-running
pgids — for the teardown sweep.

### Teardown waits for the reap, not the signal

`killpg` returns once the signal is queued, and a killed process stays in the
process table — still answering `killpg(pgid, 0)` — until its parent waits it.
So sending SIGKILL is not the same as the children being gone, and session
teardown means the latter: `reap_all_bg` signals and then waits, and only then
does the session's scratch directory get removed. Without that wait, the removal
could run against a process still holding a file open under it, and
`shut_down().await` would resolve with the processes still in the table.

The wait is on the reap itself rather than on a clock. `drain_background` is
already awaiting `child.wait()` on every background child and clears it from the
running set when that returns, so teardown re-checks on a notify fired there.
There is no interval to tune.

There is deliberately no further signal behind it. SIGKILL has already been sent, so
a group that never leaves the table is blocked in the kernel and no second signal
can move it — a bounded timer would only choose how long to wait before removing
scratch out from under a live process. The operator's remaining move is the host's:
a second shutdown signal ends the process outright.

## Spawns are serialized

Every child the crate starts — foreground and background bash, `[context]` exec,
`[env]` reference commands, stdio MCP servers, the macOS keychain's `security` —
goes through `crate::spawn`, which
holds one process-wide lock across `Command::spawn`. `spawn` creates the child's
stdio pipes and then starts the process, and on platforms without an atomic
`pipe2(O_CLOEXEC)` (macOS) the pipe is briefly inheritable between its creation and
the close-on-exec flag. A spawn on another thread inside that window gives its
child a copy of the pipe's write end; a grandchild that outlives the first command
(`sleep 30 &`) then keeps that command's stdout open, and the reader waits for EOF
until the grandchild exits. A hosted process spawns for several sessions at once,
so the window is not theoretical. The lock covers a few system calls and nothing
awaits inside it. A spawn outside the lock reopens the window for every other spawn,
so `clippy.toml` disallows `Command::spawn`, `output`, and `status` (std and tokio)
everywhere but `crate::spawn`; `crate::spawn::std_output` stands in for `output`.

## Three-places decomposition

A backgrounded command is not a single log record; it is a live entity that
decomposes into three things landing in three places. The design principle:
**specialize the ephemeral, reuse the durable**.

**The spawn** — the `bash` tool result that says "backgrounded as p#3" — is a
normal `ToolResult` payload, already logged, already projected. No new type.
Its text also states the async pattern (register a `watch_process`, end the
turn, react to the inbox match next turn): stated here, where the shape gets
chosen, because the `watch_process` result's own copy is read only after a
compressed start→watch→kill turn has been attempted, and a short process can
exit before the follow-up calls composed in the same turn reach it.

**The running tail** — the last N lines of output plus timing — is a *live,
re-sampled* view held in an ephemeral `ProcessMonitor` registry inside `Session` (a
sibling to `Inbox` and `Metrics`). It is never written to the log. It is dropped on
restart, at which point `killpg`-on-teardown reaps the children. The async
spawn/drain orchestration — which needs `Arc<Session>` and the `POST /notifications` path
— lives in `daemon.rs`; `process_monitor.rs` holds the state: the registry,
the bounded per-process capture buffer, and the read projections over it.

**The exit** — status, duration, and final tail — is a discrete immutable event.
The process monitor posts it to `POST /notifications` on slot `p#<id>`, using the control
socket just as any other client would. This folds into the existing `Ambient`
[inbox](inbox.md). Spawn = `ToolResult`, exit and pinned tail = `Ambient`, the
live state = ephemeral registry.

## The durable record — what `p#<n>` was

The three places above are what the *model* sees, and all three say the handle in
prose. That prose outlives the registry, so a reader of a recorded session meets
`p#3` with nothing to resolve it against — and recovering the mapping from
`moved to the background as p#3` is the result-text parsing `ToolTelemetry`
([tools.md](tools.md)) exists to end.

So the mapping is also recorded structurally, as `Payload::ProcessEvent`
(`src/log/process.rs`): a **mint** entry when a process enters the background
namespace, and an **exit** entry when it ends. Like `ToolTelemetry` it projects to
nothing — the wire already carries the backgrounding result and the exit
notification; this is the audit record beside them, not a second copy — so
appending one mid-session cannot move the cached prefix
([log-and-projection.md](log-and-projection.md)).

Metadata and output are **different promises**:

- **Metadata is durable, always**: the handle, the spawning `call_id`, the
  command, the spawn time, the exit status, and the duration.
- **Output stays retained best-effort**: the bounded capture with its `lost`
  count ([buffering](#buffering)) is the registry's, and dies with the daemon. A
  log-derived answer carries the metadata and says nothing about output rather
  than promising a buffer it does not have.

Both halves carry their own wall stamp instead of leaning on the entry's `at`.
The mint has to: a command adopted by the handoff is recorded when it is
*adopted*, which is later than when it spawned, and the record must not backdate
the process. The exit follows the mint so the two read the same way.

The record covers processes that reach the **background** namespace — spawned
with `background: true`, or adopted by the handoff. That is where the handle
becomes something a reader can encounter: a foreground command that completes
normally is fully described by its own `tool_use`/`tool_result` pair, and its
ordinal survives on that call's `ToolTelemetry` (`bash.handle`,
[tools.md](tools.md)) rather than in a lifecycle entry of its own.

The read side is `GET /processes/:id` ([control-api.md](control-api.md)), which
answers from the live registry when it has the process and from a fold over these
entries (`process_monitor::from_log`) when it does not — one URL for a handle whether
its daemon is running, restarted, or gone.

## Foreground → background handoff

A command is foreground-or-background at spawn time (the model's `background:
true`), but the operator can also move a **running foreground** command into the
process monitor mid-flight, instead of the only mid-run lever being kill.

The interrupt that races a running tool carries an **intent**
(`frontend::Interrupt`): `Kill` — `POST /loop/interrupt`, `killpg` the group —
or `Detach` — `PATCH /processes/:id {background: true}`, addressed at the
foreground entry the spawn already registered (the client resolves the id from
the listing; the blind verb-route this replaced could not say *what* to
detach). On detach, `Bash::run` neither kills nor reaps: it surrenders the live
child, both pipe halves, and the bytes already drained (`bash::DetachedBash`),
returned to the loop as `Outcome::Detached`. The loop calls `adopt_background`
with the entry's id, which **flips the property in place** — the id and the
capture survive; the surrendered seed bytes are deliberately not re-pushed,
because the foreground chunk sink already fed them — and spawns the same drain
task as a spawned background process. From the flip on, the two paths are
indistinguishable: tail in the triage, cursor reads, exit notification.

The handoff loses nothing and duplicates nothing: the drain select only completes a
read after its bytes are delivered to the sink, so everything read before the
detach won the race is already in the capture, and the surrendered pipes resume
from exactly that byte boundary.

The detached call's `tool_use` is paired with a **handoff result** (`is_error:
false`, unlike a kill): the model is told the user moved the command to the
background as `p#<n>`, which it can `read_process`/`kill_process` like any
other. `PATCH /processes/:id {background: true}` optionally carries a `note`, appended as a user
message right after the handoff result — direction, not tool output. A detach
request with no tool mid-run is a no-op: the wake reaches only a
currently-awaited race, and a stashed note is dropped when the next foreground
bash starts, so it can't mislabel a later handoff.

## The deadline — a foreground call may not block forever

A foreground call also detaches **on its own** once it outruns a deadline. The
timer resolves the same `Interrupt::Detach` into the same cancel slot as the
operator's, so a timer detach and an operator detach are one path; only the
handoff wording differs (`DetachedBash::auto_after` carries which it was).
Nothing is killed and no output is lost — the no-loss, no-duplicate property above is what makes
that true.

The deadline exists because a foreground call waits for its **output** to close,
not for its direct child to exit. A command that starts a server with a shell
`&` leaves a grandchild holding the pipes open, so the call never reaches EOF,
and both recovery levers are operator-side over the control channel — an
unattended autonomous session has no way back. One such command ends a
session.

Two numbers, `[bash]` in the config (`config::BashLimits`), because one value
cannot be both:

- `background-after` — the default, applied when a call names nothing (300s).
- `background-after-max` — the clamp on a call's own value (1800s). This is the
  safety property, which is why a call cannot raise itself past it.

The `bash` tool takes an optional `background_after` in seconds, clamped to the
max. It is an **expectation, not a promise**: a caller states what it believes an
upper bound to be, and exceeding it is a signal the caller asked for rather than
a failure. It is emphatically not a timeout — a model that reads it as a
kill-deadline pads it defensively and erodes the ceiling, so both the field name
and its description say what actually happens.

An auto-detach handoff names the way out — `background_after`, `background:
true`, and `read_process <handle>` — because the failure it replaces was
*silence*, which taught nothing: three sessions across two models hit the hang
and none of them discovered `background: true`.

## Why the running tail is not a notification

The [inbox](inbox.md) / notification model is built on immutable bodies: an
`Ambient` body is frozen at first-show, and a "left" item re-emits verbatim.
Supersession only touches the queued slot notifications; refreshing an already-shown body is
unbuilt by design (the projection invariants depend on immutable bodies).

A tail's whole value is that it *updates*. Forcing it through notifications would
pile up one stale, frozen snapshot per process per triage — defeating the purpose.
The escape is to not log the tail at all: render it live from the registry at each
triage, in its own triage section, and materialize a durable record only when the model
explicitly pins it.

There is also a structural constraint: output arriving after the spawn's `tool_use`
has already been answered has no `tool_use` to pair with, so it *cannot* be a
`ToolResult`. Late output is structurally a live view or a notification — never a
triaged result. The same argument applies to exit events, which is why the exit
reaches the model through the Ambient inbox. The `ProcessEvent` record above is
not a second route to the model: it projects to nothing, and exists for readers
that arrive after the session.

## One registry, two projections

The `ProcessMonitor` registry is read two ways — ambient awareness
and directed query — mirroring the [inbox](inbox.md)'s own ephemeral-queue vs.
logged-Ambient split.

**Ambient** — every triage auto-shows a `background` section for each running,
un-muted process: `[p#<id>]` with a fresh tail, running time, and idle time
(`running 4m12s · idle 8s`; "idle" = `now − last_chunk_at`, stamped in the drain
loop). This is awareness that arrives *while the model is doing something else*.

**Directed** — `list_processes` and `read_process(handle, cursor)` are normal read
tools whose results ride the normal triage: the model reaching for a process on
purpose. The directed `list_processes` view includes muted processes (annotated),
because the model asked explicitly — the ambient auto-show hides muted processes.
This retires the `bash(echo "")` antipattern (issuing a no-op to trip a triage just
to see notifications). The missing piece was a *direct projection*, not another poll
verb.

**Two views of one registry.** `BgView` — built under the registry lock — is the
*model-facing* snapshot: tail inline, times as humanized elapsed counts. A triage is
synchronous and point-in-time, so the model has no stream to subscribe to and the tail
must be there or it does not exist; and absolute Unix millis are unreadable in a prompt.

`ProcessState` is what `GET /processes` serves and the control API's `processes`
topic carries — one representation for both, so they cannot drift — and it
diverges from `BgView` on both fields for the mirror-image reason
([control-api](control-api.md)). It has **no tail and no last-chunk stamp**: both
change with every output chunk, and that topic is level-triggered, so carrying
them would emit a whole-topic replacement per chunk — worse than the poll it
replaces. That content rides the ephemeral `output` class as `process_output`
deltas, where high-rate and lossy is the contract, and a client opening cold
seeds itself from `GET /processes/:id/output` rather than from the topic. Times
are absolute so a client computes elapsed without the daemon re-emitting as the
clock moves. Unlike the model-facing views, it lists **every** running process —
foreground and muted included, flagged rather than filtered — because the
resource is the whole truth and hiding is a per-client rendering choice.

What survives in `ProcessState` changes only on spawn, exit, mute, or the
handoff flip — which is what makes `?include=state:processes` viable on a
battery-powered client. The two types were one only while a poll made the
economy free.

## Triage of a live process

The triage tool carries a `background` verdict keyed by handle (distinct
from the `inbox` verdict keyed by arrival seq — live handles and frozen arrival seqs
are different identity models). Three dispositions:

**pin** — snapshot the *current* tail, log it as one `Ambient`, and pin its seq in
the same `Triage`. Projects as `⟢ p#3 (cargo watch): <tail>` at the `Triage`
position. This is cache-safe tail-ward growth. Pinning again later appends a newer
snapshot — building the honest `running…→finished…` timeline. `pin_snapshot` in
`process_monitor.rs` produces the `(source, body)` pair ready to wrap in a
`Notification`.

**mute** — stop surfacing the running tail in the ambient triage; the process keeps
running, and its *exit still notifies*. Killing is separate and explicit
(`kill_process(handle)`), never conflated with "stop showing me this."

**leave** (the default when unlisted) — re-show a fresh tail at the next triage.

## Idle-triggered triage

A triage fires on a tool result, so without an explicit mechanism an idle agent cannot
perceive an arriving event without manufacturing a spurious tool call. The fix: when
the loop is parked, an arriving event wakes a **triage that rides no tool
result** — the inbox/background section plus an ask for the triage-only tool (`keep`
absent; nothing to retain), appended as a plain user message; like every triage the
ask rides the tail instruction, never a forced `tool_choice`
([triage.md](triage.md), the shared envelope). It is a throwaway
fork: it logs `Ambient` + `Triage` (audit grows) but **projects to nothing unless
it pins** — the prefix stays byte-identical and cache-hot.

**The trajectory rule: a main-trajectory turn runs if and only if the triage changed
the projection — i.e., pinned something.** A clean exit that gets dismissed or left
produces no wire change; the loop returns to idle. A failed exit that gets pinned
lands the `⟢ …` block on the wire; a real turn runs so the model acts. A pin with
no actionable work costs one cheap ack turn; the model avoids it by leaving
"noted, no action" items.

Throttles prevent idle-triage from thrashing: **debounce** coalesces events in a
small window into a single triage; **urgency** lets low-urgency events wait for the
next natural triage rather than waking a fork. Because tails
are *not* notifications, idle-triage fires only on discrete lifecycle events,
operator nudges, and inter-agent messages — low-frequency by construction. Had tails
been notifications, every refresh would wake a fork.

When `wait()` returns, its ordinary tool-result triage renders the same pending inbox
items for triage. The wait does not drain or disposition them; it only keeps the turn
open until one arrives.

## Tool surface

`bash` gains `background: true` as an optional boolean input. The command is the
same, the approval gate is the same; the loop registers the child in the `ProcessMonitor`
instead of awaiting it and returns the `p#<id>` handle immediately.

The process monitor verbs: `list_processes`, `read_process(handle, cursor)`,
`kill_process(handle)`, `watch_process(handle, pattern)` ([watches](#watches)),
`check_inbox` (surface pending inbox notifications on demand), and `wait()` (block
until the next notification and return the pending notifications; refuse without a
known wake source), and `ask(question)` ([below](#ask--a-question-that-blocks)).
Tool definitions are constant `json!` values with sorted keys so
the `tools[]` array stays byte-stable across turns (see [transport](transport.md) for
why this matters for prefix caching).

## `ask` — a question that blocks

`ask(question)` puts a question to the agent that supervises the session when there is
one, otherwise to the operator, and blocks until the reply, which returns as the call's
result. The question reaches the supervisor as the session's `asking` status, with the
`message` call that answers it ([tools.md](tools.md)); the operator's clients show it
either way. It exists so that ending a turn has one meaning — the work
is done — and a question has its own spelling (`agent/ask.rs`, `daemon/ask.rs`). It is
always advertised and skips the permission cascade: it acts on nothing, and asking
permission to ask would gate a question behind a question.

- **A reply resolves it.** While an `ask` is pending, a non-blank `POST /turns` that
  names the question resolves it: `answers` on the route, `reply_to` on `message`
  (`Session::submit_named_turn`, docs/control-api.md). A turn naming none is refused
  with the question, from every client; the TUI and the web client name the question
  they show, and a send refused because a question arrived first returns to the
  composer. The turn becomes the call's
  `ToolResult` and is never queued or logged as a `User` entry. The result entry carries
  the submission's `submission_id` and `source`, and a sourced reply projects as `[seq]
  source: text`, as a sourced user turn does. A notification does not resolve it. The
  operator's reply releases a hold; an agent's leaves it in place, and the loop waits at
  its next juncture (docs/control-channel.md).
- **Turns queued before it are recalled.** They were written before the question
  existed, so `begin_ask` withdraws them from the queue as `DELETE /turns/:id` would and
  publishes them on the `ask` state topic as `recalled`; the submitting client puts them
  back in its composer. An agent that sent one is told instead: the asking status carries
  the returned messages (`Condition::Asking::returned`), and its rendering for the
  reader who sent one adds `returned: your message "…" was returned undelivered; answer
  first, then resend it if it still applies`. A subscribed session reads it in the status
  posted to it, an MCP mount in `status`, `wait` and the status stream (matched by the
  `mcp[<client>]` source, so two mounts with one client name both read it), and a
  session that drives the asker without a subscription is woken with the same status
  (`Host::post_status`, `daemon/subscriptions.rs`).
- **Notifications do not interrupt it.** The reply bypasses the triage (`triage::bypassed`
  names its shape `operator reply`); the inbox is still triaged. In a batch with other
  results the triage runs as usual with a stand-in for the reply. When every result in the
  batch bypasses, the loop appends them and then runs the standalone triage that
  idle triage uses (`agent::triage_and_record`) — which, unlike the per-result triage,
  does not hold on failure: notifications stay pending for the next triage.
- **Several asks in one completion are put one at a time**, in call order, each carrying
  its position (`n` of `of`). The tool description tells the model the order is
  unspecified, so they should be independent. A driving agent (a parent session, an MCP
  client) answers them one `message` per question, each naming its question's handle,
  reading the next from the status in between; `efference control message
  --reply-to` does the same from a shell. There is no batched answer. On the wire each reply opens `asked: <question>`,
  so the model matches answers to questions without relying on order.
- **Interrupts** treat it as any in-flight call: the ask ends with an error result, the
  rest of the batch is aborted, and a reply that raced the interrupt is resubmitted as
  an ordinary turn.

A pending ask is a decision point: the loop phase is `asking` (docs/control-api.md),
so `is_settled` holds, the `ask` topic carries the
question, the session's status is `asking` (waking a subscribed parent as a gate does,
[tools.md](tools.md)), and a push goes out as for a turn end.

## Watches

`watch_process(handle, pattern)` registers a pattern against a running process and
returns immediately. Nothing blocks: the drain scan matches the process's output as
it arrives and posts a hit to the [inbox](inbox.md), where it reaches the model the
same way an exit does — at the next triage, or by waking an idle agent. It is the
pattern half of wake-on-notification; the exit half needs no tool at all.

Four decisions define what a watch is:

- **The pattern is a plain substring, matched per line, case-sensitively.** The
  question a watch answers is "wake me when this stream prints X", and X is almost
  always a literal the model just read in the command's own output (`Compiled`,
  `Listening on`, `test result:`). A regex would add an expression the model can get
  subtly wrong — and get wrong *silently*, since a watch that never fires looks
  exactly like a process that never printed. Substring has one failure mode and it
  is the obvious one.
- **A watch fires once, then it is gone.** The wake case is "tell me when this
  happens", not "tell me every time". A repeating watch on a chatty stream would
  wake the idle triage fork per matching line, which is the thrash idle-triage's
  debounce exists to prevent. Re-arming is one tool call, and the notification body
  says so.
- **Only output printed after registration matches.** The scan mark advances on
  every chunk whether or not anything is watching, so a pattern already sitting in
  the retained capture — which the model has usually just read, since reading is
  what prompted the watch — cannot fire an immediate stale hit. A complete line is
  scanned once; the trailing partial line is scanned without advancing the mark, so
  a command that prints a prompt and waits is still watchable.
- **The notification carries the handle, the pattern, and the matching line**, on
  the plain `p#<n>` source the exit notification also uses — one `⟢ p#3: …` shape
  for everything about a process. It carries **no slot**: a once-firing hit has
  nothing to supersede, and the `p#<n>` slot belongs to the exit notification, which
  a hit must neither replace nor be replaced by.

A watch on an unknown or already-exited process is refused rather than accepted:
there will be no more output to match, so an accepted watch would never fire. A
watch that is still armed when the process exits dies with it — the exit
notification is what arrives instead.

## Buffering

Each process has one bounded append buffer (`BUFFER_CAP` bytes). When the cap is
exceeded, the oldest bytes are dropped from the front and the count of dropped bytes
is recorded in `dropped`. No truncation is ever silent: the tail text and
`read_since` results both surface a breadcrumb — "… [N earlier bytes dropped at the
cap]" — when content has been lost.

The ring tail and the `read_since` cursor are two views of the same buffer. The
cursor is a byte offset into the *logical* stream (total bytes ever received,
including dropped). `read_since` clamps reads to what is still retained, reports any
bytes lost before the cursor, and returns a new cursor so the model pays only for
the delta on subsequent reads.

## Terminal formatting

Processes write to pipes, not a terminal. A program that emits colour and cursor
escapes, hyperlinks, or carriage-return redraws anyway (one that forces colour, or
misreads its output as a terminal) produces bytes that format nothing for a reader of
its output, so the text the agent reads drops them. The capture keeps the bytes as written, and
the routes that serve it to clients (`GET /processes/:id/output`, the `process_output`
deltas) return them unchanged; the clients clean them for display themselves.

Every text the agent reads is made by `process_monitor::strip_formatting`, a pure
`&str -> String`: a synchronous `bash` result (the `RawResult` that is logged and that
triage reads), the `read_process` body, the tail in the ambient triage and in a pin,
the exit notification's final output, and the line a watch hit quotes. Watch patterns
match the cleaned line. The function removes CSI sequences, OSC strings (an OSC 8
hyperlink keeps its visible text and drops the target), other `ESC` sequences, and, in
each line, everything before the last non-empty carriage-return segment. A `\r\n`
ending is a line ending. It does not emulate a terminal: nothing is positioned or
overdrawn. Text without escapes or carriage returns passes through byte-identical.
A synchronous `bash` call has no capture
beside its result, so its escapes are kept nowhere. Results logged before the function existed are not rewritten.
