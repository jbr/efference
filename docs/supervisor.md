# Background processes — the supervisor

## The poll-loop antipattern and the fix

The poll-loop antipattern ("check, spin, check") emerges when the tool vocabulary
only offers "check," forcing the model to be its own scheduler. The fix is
**wake-on-notification**: the model never blocks waiting for a process. It ends its
turn, goes idle, and is woken when something worth its attention happens — an exit,
a matched pattern.

This is strictly better than a blocking `wait(until=…)` call. A blocked wait makes
the agent inert: it cannot react to a *different* process failing while it waits,
and it holds an open turn for nothing. "Wait for exit" is simply the idle agent
being woken by the exit notification (the idle-triggered triage gate, below). "Wait
for a pattern" is the non-blocking `watch_process(handle, pattern)` — the drain task
matches output and fires a notification — which composes with wake-on-notification
instead of fighting it (see [watches](#watches), below).

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

- **In prose** — tool results, the peek, `list_processes`, notification sources —
  it renders as `p#<n>` (`supervisor::handle`). The prefix distinguishes it from an
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
- **process → call.** A client showing a live process row (the TUI sidebar's
  `PROCESSES`, the web tasks pane) can jump to the transcript entry the process came
  from, rather than leaving the operator to find it by reading commands.

`call_id` is optional in the type because the registry does not require a call to
register a process; every process the harness spawns today has one.

The **OS pid is an attribute, not the identifier**: the `processes` topic carries
it so a client or the agent can reach the process directly (signals, `lsof`) when
the supervisor doesn't model what it needs. It cannot be the identity because pids
are recycled and the log is durable — a logged pid stops resolving after reuse,
resumption, or a reboot.

The mint counter **seeds from the log at boot** (`max_handle_mention` over each
serialized entry). Today that scan always runs over an empty log — `serve` mints
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
is shared between the foreground abort path and the supervisor's kill/teardown path.
The supervisor registry exposes `running_pgids()` — the set of all still-running
pgids — for the teardown sweep.

## Three-places decomposition

A backgrounded command is not a single log record; it is a live entity that
decomposes into three things landing in three places. The design principle:
**specialize the ephemeral, reuse the durable**.

**The spawn** — the `bash` tool result that says "backgrounded as p#3" — is a
normal `ToolResult` payload, already logged, already projected. No new type.

**The running tail** — the last N lines of output plus timing — is a *live,
re-sampled* view held in an ephemeral `Supervisor` registry inside `Shared` (a
sibling to `Inbox` and `Metrics`). It is never written to the log. It is dropped on
restart, at which point `killpg`-on-teardown reaps the children. The async
spawn/drain orchestration — which needs `Arc<Shared>` and the `POST /notifications` path
— lives in `daemon.rs`; `supervisor.rs` holds the state: the registry,
the bounded per-process capture buffer, and the read projections over it.

**The exit** — status, duration, and final tail — is a discrete immutable event.
The supervisor posts it to `POST /notifications` on slot `p#<id>`, using the control
socket just as any other client would. This folds into the existing `Ambient`
[inbox](inbox.md) with no new `Payload` variant. Spawn = `ToolResult`, exit and
pinned tail = `Ambient`, the live state = ephemeral registry.

## Foreground → background handoff

A command is foreground-or-background at spawn time (the model's `background:
true`), but the operator can also move a **running foreground** command into the
supervisor mid-flight, instead of the only mid-run lever being kill.

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
indistinguishable: tail in the peek, cursor reads, exit notification.

The seam loses nothing and duplicates nothing: the drain select only completes a
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
Nothing is killed and no output is lost — the seam property above is what makes
that true.

The deadline exists because a foreground call waits for its **output** to close,
not for its direct child to exit. A command that starts a server with a shell
`&` leaves a grandchild holding the pipes open, so the call never reaches EOF,
and both recovery levers are operator-side over the control channel — an
unattended autonomous session has no way back. One such command used to end a
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
Supersession only touches the un-shown queue; refreshing an already-shown body is
unbuilt by design (the projection invariants depend on immutable bodies).

A tail's whole value is that it *updates*. Forcing it through notifications would
pile up one stale, frozen snapshot per process per gate — defeating the purpose.
The escape is to not log the tail at all: render it live from the registry at each
gate, in its own peek section, and materialize a durable record only when the model
explicitly pins it.

There is also a structural constraint: output arriving after the spawn's `tool_use`
has already been answered has no `tool_use` to pair with, so it *cannot* be a
`ToolResult`. Late output is structurally a live view or a notification — never a
distilled result. The same argument applies to exit events, which is why exit routes
through the Ambient inbox rather than a new payload type.

## One registry, two projections

The `Supervisor` registry is read two ways — ambient awareness
and directed query — mirroring the [inbox](inbox.md)'s own ephemeral-queue vs.
logged-Ambient split.

**Ambient** — every peek auto-shows a `background` section for each running,
un-muted process: `[p#<id>]` with a fresh tail, running time, and idle time
(`running 4m12s · idle 8s`; "idle" = `now − last_chunk_at`, stamped in the drain
loop). This is awareness that arrives *while the model is doing something else*.

**Directed** — `list_processes` and `read_process(handle, cursor)` are normal read
tools whose results ride the normal gate: the model reaching for a process on
purpose. The directed `list_processes` view includes muted processes (annotated),
because the model asked explicitly — the ambient auto-show hides muted processes.
This retires the `bash(echo "")` antipattern (issuing a no-op to trip a gate just
to see notifications). The missing piece was a *direct projection*, not another poll
verb.

**Two views of one registry.** `BgView` — built under the registry lock — is the
*model-facing* snapshot: tail inline, times as humanized elapsed counts. A peek is
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

The triage gate tool carries a `background` verdict keyed by handle (distinct
from the `inbox` verdict keyed by arrival seq — live handles and frozen arrival seqs
are different identity models). Three dispositions:

**pin** — snapshot the *current* tail, log it as one `Ambient`, and pin its seq in
the same `Triage`. Projects as `⟢ p#3 (cargo watch): <tail>` at the `Triage`
position. This is cache-safe tail-ward growth. Pinning again later appends a newer
snapshot — building the honest `running…→finished…` timeline. `pin_snapshot` in
`supervisor.rs` produces the `(source, body)` pair ready to wrap in a
`Notification`.

**mute** — stop surfacing the running tail in the ambient peek; the process keeps
running, and its *exit still notifies*. Killing is separate and explicit
(`kill_process(handle)`), never conflated with "stop showing me this."

**leave** (the default when unlisted) — re-show a fresh tail at the next gate.

## Idle-triggered triage

A gate fires on a tool result, so without an explicit mechanism an idle agent cannot
perceive an arriving event without manufacturing a spurious tool call. The fix: when
the loop is parked, an arriving event wakes a **triage gate that rides no tool
result** — the inbox/background section plus an ask for the triage-only tool (`keep`
absent; nothing to retain), appended as a plain user message; like every peek the
ask rides the tail instruction, never a forced `tool_choice`
([redaction-fork.md](redaction-fork.md), the shared envelope). It is a throwaway
fork: it logs `Ambient` + `Triage` (audit grows) but **projects to nothing unless
it pins** — the prefix stays byte-identical and cache-hot.

**Escalation contract: a main-trajectory turn runs if and only if the triage changed
the projection — i.e., pinned something.** A clean exit that gets dismissed or left
produces no wire change; the loop returns to idle. A failed exit that gets pinned
lands the `⟢ …` block on the wire; a real turn runs so the model acts. A pin with
no actionable work costs one cheap ack turn; the model avoids it by leaving
"noted, no action" items.

Throttles prevent idle-triage from thrashing: **debounce** coalesces events in a
small window into a single triage; **urgency** lets low-urgency events wait for the
next natural gate rather than waking a fork. Because tails
are *not* notifications, idle-triage fires only on discrete lifecycle events,
operator nudges, and inter-agent messages — low-frequency by construction. Had tails
been notifications, every refresh would wake a fork.

## Tool surface

`bash` gains `background: true` as an optional boolean input. The command is the
same, the approval gate is the same; the loop registers the child in the `Supervisor`
instead of awaiting it and returns the `p#<id>` handle immediately.

The supervisor verbs: `list_processes`, `read_process(handle, cursor)`,
`kill_process(handle)`, `watch_process(handle, pattern)` ([watches](#watches)),
and `check_inbox` (surface pending inbox notifications on
demand, so the model need not wait for them to ride its next action). Tool
definitions are constant `json!` values with sorted keys so the `tools[]` array
stays byte-stable across turns (see [transport](transport.md) for why this matters
for prefix caching).

## Watches

`watch_process(handle, pattern)` registers a pattern against a running process and
returns immediately. Nothing blocks: the drain scan matches the process's output as
it arrives and posts a hit to the [inbox](inbox.md), where it reaches the model the
same way an exit does — at the next gate, or by waking an idle agent. It is the
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
