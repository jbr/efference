# Control channel & the Frontend trait

## The singular input source

The HTTP-over-Unix-socket interface is not merely equivalent to other input paths — it is the only input path. There is no privileged in-process route. The loop's sole `Frontend` implementation is `RemoteFrontend`, which reads from shared daemon state populated exclusively by HTTP handlers. The human using the TUI client, an automated driver (such as Claude Code), and a subagent are all HTTP clients of the same socket. The one added hop — the approval round-trip — is microseconds against inference latency and the Anthropic round-trip, so it costs nothing in practice.

The TUI is not a special client with privileged access; it is one client among peers. Two payoffs follow. First, any driver can exercise the full harness without a human hand-off — the control channel is the same surface regardless of who is on the other end. Second, subagents need no separate mechanism: the subagent boundary is the client boundary — an efference agent's tool can spawn and drive another efference daemon over the same socket interface. Agents addressing each other is just one agent injecting an `Ambient` event into another's log, triaged at its head (see [the inbox](inbox.md)) — no separate messaging system is needed.

## The log is already the event store

SSE has no built-in replay, but none needs building: the append-only log with monotonic `seq` is the event store, so a reconnecting client replays from a cursor — the standard `Last-Event-ID` pattern, with the log as the buffer (see [log-and-projection.md](log-and-projection.md)).

`GET /events` tails the log as a Server-Sent Event stream. Each event carries `id: <seq>` and `data: <serialized LogEntry>` — the same JSON `GET /log` returns, so the tail and the catch-up read are byte-identical per entry. A reconnecting or late-joining client replays history with `GET /log?since=<seq>` and then resumes the SSE tail, deduplicating by `seq`. The slow-consumer case is also safe: because the log is the buffer and each `GET /events` connection reads at its own pace, a slow client never drops events (unlike a broadcast channel).

Ephemeral control state — a pending approval, current status, live process tails — is not a log payload. The log records the *result* of a gate (approved and ran, or rejected), not the in-flight gate itself. A connecting client therefore does three things in sequence: `GET /state` to fetch current ephemeral state (last `seq`, any pending approval, usage telemetry, running background processes), `GET /log?since=<seq>` to catch up on history, then attach to `GET /events` and deduplicate by `seq`.

## Session: session state and control

Every `Session` carries two absolute directories, stored immutably as a
`policy::Directories`: its **project** (config, the session dir, grant keys,
`[context]`) and its **working directory** (tools, the paths a call names, the
directory a child inherits), exposed by `directories()` and `cwd()`. They are the same
directory unless the session works in a git worktree ([worktrees.md](worktrees.md)).
Policy classification, config-write checks, missing-grant diagnosis, probe-ledger
replay, and foreground/background bash spawning use them rather than the process cwd.
`Toolbox::bash_and_mcps` supplies the working directory to `Bash`; a foreground child
keeps it when adopted into the process monitor. Relative config-source paths are joined to
the project before the live config file is opened. The host derives both from the
creation request's `cwd` before bootstrap. No session changes the process working
directory.

Boot also captures an in-memory base environment for provider lookup, reference
commands, the session's `EnvPolicy`, and every child spawn. Config edits reuse that
base for masking rather than sampling the host environment. Child inheritance is
replaced before policy and harness-owned values are applied ([env.md](env.md)).

`Session` represents one session's state and control surface. The agent loop task and HTTP handlers both hold it as `Arc<Session>`. The daemon registers it as trillium's server-global shared state; handlers retrieve it per-connection. Listener ownership lives in `src/daemon/serve.rs`; the session's own tasks are owned by a `SessionRuntime` (below), not by the type itself. Its contents:

**`Arc<Mutex<Log>>`** — the persisted, append-only log (see [log-and-projection.md](log-and-projection.md)). The loop appends under this lock via `Session::append`, which also pings the append notifier so SSE tailers wake. The lock is never held across an `await`; code that needs to call the model on a snapshot calls `Session::snapshot()` to clone the entries first.

**Turn inbox** — an unbounded MPSC channel for delivery, plus a `Mutex<VecDeque<SubmittedTurn>>` that is the **authority** on what is pending. `POST /turns` records the turn in the queue and sends it down the channel; `RemoteFrontend::read_user_turn()` awaits the next pop and then **claims** it against the queue, dropping a turn that is no longer there. That split exists because a channel cannot be searched, and the queue must be: `GET /state`'s `queued` topic reads it, and `DELETE /turns/:submission_id` withdraws a suffix from it (recall — [control-api.md](control-api.md)). A withdrawn turn still surfaces on the channel, fails its claim, and is dropped unappended, so it is never logged and there is nothing to retract. This also seeds turn 1: an idle daemon parks at `read_user_turn` waiting for the first `POST /turns`, which retired argv-seeding and the "backgrounded run can't get its opening prompt" problem. A turn submitted while the loop is *mid-trajectory* doesn't wait for the whole trajectory to end: `RemoteFrontend::drain_queued_turns()` non-blockingly pulls the queued turns at the next juncture (top of the inner loop, after the last tool's results land) and the loop appends each as a canonical `Payload::User` — mid-task delivery ([operator participation](#operator-participation--the-intervention-points)). A user turn is undismissable; it never routes through the inbox/triage path. While an `ask` is pending, a turn naming it is diverted to it as the call's reply instead of queued, and releases the hold when it is the operator's ([process-monitor.md](process-monitor.md)).

**Loop phase** — a `Mutex<LoopPhase>` (`src/daemon/phase.rs`): where the loop is, as one exclusive value, published as the `loop_phase` topic ([control-api.md](control-api.md)). The loop sets it from its own control flow: `requesting` when it claims a turn, takes a continue, starts an idle triage, or sends a request or a triage; `executing` when a batch of calls starts; `idle` when it parks at `read_user_turn` (`going_idle`). Each wait names itself for as long as it lasts through a `PhaseWait` guard that ends into the phase the loop moves to next — `delayed` at a finite pace sleep, `blocked` at a held pace, a config-repair park, or a held triage, `cold_parked` at the cold check, `gated` once a gate is registered, `asking` once an `ask` is stored. A decision that ends a wait from outside the loop moves the phase itself, at the moment the route takes it (`Session::end_wait`): `resolve_approval`, an answer to an `ask`, a release or submit at a pace wait, a person's action at the cold park, a repaired config, a continue at a held triage. That ordering is what keeps a `settle` that races the loop's wake from reading a wait already answered. The pause between upstream attempts is the one phase not stored: the retry cell marks it (`UpstreamRetryCell::delay`), because the triage's attempts run where no `Session` is held, and `loop_phase()` reads a `requesting` loop as `delayed` while the mark is set. A detached title ask retries on the same cell with the unmarked `pause`, since its wait is not the loop's. `turns_ended` counts trajectories that went idle, which is how a status subscriber tells a finished turn from an idle triage that pinned nothing.

**Pending-approval registry** — a `HashMap<CallId, oneshot::Sender<Approval>>`. When the loop reaches a tool call requiring approval, it calls `Session::register_pending`, which creates a oneshot channel, stores the sender keyed by the call's `CallId` (the same id already on the wire — no new id scheme), and returns the receiver. The loop blocks on that receiver. `PUT /approvals/{call_id}` looks up the sender and fulfills it.

**Interrupt `Notify` (×2) + latch + detach note** — `POST /loop/interrupt` fires the kill notifier, `PATCH /processes/:id {background: true}` fires the detach notifier (plus stashes its optional note in a `Mutex<Option<String>>`); `RemoteFrontend::interrupted()` selects over both and resolves with the intent (`Interrupt::Kill` or `Interrupt::Detach`). The loop races this against a running tool: kill is the SIGINT equivalent (process group killed), detach is the foreground→background handoff (the live child adopted into the process monitor, see [process-monitor.md](process-monitor.md)). The kill notifier is an `Arc<Notify>` because the agent client's read loop races it too (`Client::set_abort`): an interrupt mid-generation ends the stream ([the interrupt](#the-interrupt)).

`POST /loop/interrupt` *also* sets an **`interrupt_latch: AtomicBool`**, and that is what makes it reach tools the race can't. `Notify::notify_waiters` wakes only currently-registered waiters and stores no permit; only the bash backend awaits the abort future (`src/tools.rs` — the MCP providers are synchronous and resolve whole), so during any other call the wakeup has nowhere to land and is dropped. The latch survives it. See **[the interrupt](#the-interrupt)** below for what the loop does with it.

**Pace state** — `Mutex<Pace>` (`{ speed: Duration, held: bool, held_back: Vec<SubmittedTurn> }`) plus a `pace_interrupt` `Notify` ([operator participation](#operator-participation--the-intervention-points)). The operator's between-turns wall-clock dial: `effective_sleep = held ? ∞ : speed`, `held` an overlay that retains `speed` underneath (resume returns to it, never to zero). `Session::pace_at_juncture` applies an interruptible sleep at the between-step juncture; `set_pace` (via `PATCH /loop {pace}`) moves the dials, and a **release** or a **submit** fires `pace_interrupt` so the wait ends immediately. A hold ends only on a release — `pace`, or a turn from one of the operator's clients, which releases it — so a queued turn does not end it. An agent's turn (`names::sources::agent_source`: `session[…]`, `mcp[…]`) submitted while held goes into the queue, where `queued` shows it and recall can take it back, and into `held_back` rather than down the channel; the release sends `held_back` on in submission order, under the pace lock, so a turn cannot be held back after the release that should have sent it. An idle loop whose queue holds only such turns reads as settled: it waits on the operator. It is **operator-view only — never a log entry, never projected**, so a held or slowed loop is byte-identical on the wire to a fast one (that, plus timestamps-never-projected, is the whole invisibility guarantee: loud to the operator's TUI, silent to the agent).

**Append `Notify`** — pinged by every `Session::append` call; the `GET /events` tail arms a `Notified` before each catch-up read so an append landing between the read and the await cannot be lost (the lost-wakeup-safe pattern).

**Broadcast for stream deltas** (`broadcast::Sender<StreamDelta>`) — carries ephemeral pre-commit output fragments to the control stream's `output` class. Lossy by design: a consumer that lags past the ring drops the oldest deltas rather than stalling the loop. The committed entry on the `log` class is the authoritative copy; the durable log is the backstop.

## Session runtime: one session's tasks and their shutdown

`SessionBoot` (`src/daemon/runtime.rs`) is the pure-data set of inputs a session
starts from: the provider client, the mounted toolbox, the triage, the spotter,
the log, the session paths, the loaded role, the review level, the model's
capabilities and name, the environment policy, and whether to keep scratch. Whoever
creates the session resolves these; nothing in the struct reads ambient process
state. A host's session-creation handler builds one; the
same struct.

`SessionRuntime::start(boot, parent)` builds the `Session`, sets its boot-time
fields (review level, capabilities, reservation, identity, cost tracking, usage log,
tool catalog), and runs the session's tasks under a child `Swansong` of `parent`.
The topic differ, cache keepalive, config watcher, attachment watcher, and the
agent loop itself are all spawned on the current tokio runtime — the host's
multi-thread runtime — each wrapped in the child's `interrupt`. The loop is `Send`
end to end: embedded MCP dispatch is async with `Send` futures over cloned state, and
the `Frontend`/`Spotter` futures carry a `Send` bound.
Every session therefore shares one I/O driver with the host and with every other
session, which is what makes the process-wide HTTP client's connection pool
(`src/http.rs`) safe to share: a pooled connection's driver task is always on a
runtime that keeps being polled. (A loop on a runtime of its own stopped being polled
whenever it blocked synchronously — an in-session `agents__create` waiting on the
host — and a pooled h2 connection that loop had opened then hung any host-side
request that borrowed it.) The in-session host calls that wait (`create`, `archive`) are
awaited on this same runtime, like everything else the loop does.
Handlers and watchers reach the session through `Arc<Session>`.

The child `Swansong` is the runtime's one control handle. `shut_down()` on it
cancels the loop and the watchers and completes only after teardown has run: the
loop task holds a guard until it has reaped background process groups and removed
the scratch dir (unless `keep_scratch`). Shutting down the parent stops every
runtime under it the same way; shutting down one runtime does not touch its
siblings or the parent. `finished()` waits for the loop to return — on its own or
by cancellation — and yields its outcome. `serve_host` creates the parent
`Swansong`, hands it to the listeners (so SIGINT/SIGTERM reach the loop) and starts
one runtime under it; the two-runtimes test in `src/daemon/test/runtime.rs` pins
the isolation.

Session preparation — from a `SessionRequest` (the creation body whole, with the
root, base environment, session dir and model selections the host resolved from
it) to a `SessionBoot` — is `boot::prepare`
(`src/boot.rs`): the origin config and its `[env]` references, the provider and
capability lookup, minting the session dir and snapshotting the config, scratch,
the `[env]` policy, the log, the toolbox and MCP mounts (`src/boot/mcp_mount.rs`,
the one step that may wait for a human), the frozen preamble, the client, the
spotter, and the triage. The CLI builds the request from flags; a host's
creation handler builds it from a request body. Boot progress prints to stderr
today. Listener binding stays with whoever runs the runtime.

Everything that can be judged without persistent allocation is judged before the
mint: the config, its `[env]` references, the capability lookup, the temperature,
and the effort level with its thinking coupling — the level is read from the
origin, which the session's snapshot copies verbatim. A failure after the mint
rolls back what that attempt minted: the session dir (with the config snapshot
and the log inside it) and the scratch dir, under `--keep-scratch` the scratch is
left as a post-mortem artifact. What may be rolled back is exactly what an
exclusive `create_dir` returned success for in this call, which is what makes the
path this attempt's and no one else's; a refused mint records nothing and removes
nothing, so a collision with an existing session or a racing creation leaves that
session's files untouched. Spawned stdio MCP servers need no rollback entry: the
child is killed when the toolbox holding it drops, which the error return does.
The net effect is that a rejected creation frees its name for a corrected retry
instead of making the retry collide with its own wreckage. The runtime owns the
session's lifetime from `start` to teardown and nothing outside it.

## The host: several sessions in one process

`Host` (`src/daemon/host.rs`) is a map of session ids to started runtimes under
one root `Swansong`; `serve_host` (`src/daemon/serve.rs`) binds the control socket (`~/.efference/host.sock`,
or the path `efference --host --socket` names) and the optional TCP listener with that
`Swansong`,
so a signal stops the listeners and every session. `Host::adopt` registers a
runtime started under the host's `Swansong` and spawns a watcher that removes the
entry once the runtime has finished, whether by `DELETE /sessions/:id`, its own
end of input, or host shutdown. That watcher is the host's supervision of session
completion.

An archive (`Host::end`, `src/daemon/host/end.rs`) and the watcher run at once: the
end initiates the shutdown, and the watcher wakes when the loop returns, posts the end
to subscribers, and ends the session's non-detached children while the end retires the
worktree and records the archive. The watcher's removal is ordered after the end's
record, and the end's resolution after the removal, by the entry's `Departure`: an
end is counted on the entry in the critical section of the map lock that finds the
entry, until it has appended `Archived` (or discarded the session) and closed the log;
the watcher removes the entry only in a critical section that finds no end counted,
and otherwise waits for the count to reach zero; and an end resolves once the entry
has gone. A client that no longer finds a session in the listing finds `Archived` in
its log, and an archive answers after the sessions that end with it have left, since
the watcher's cascade awaits their ends. With no end under way, as when a session's
loop fails, the watcher removes the entry as soon as its cascade has run.

Routing is per-conn state: the `/sessions/:session/*` stage looks the id up, puts
the session's `Arc<Session>` (and its `Files` view) into conn state, and dispatches
into the unchanged control routes, whose one accessor reads conn state first and
the server-global session second. A session bind still registers its session as
server-global state; a host registers the `Arc<Host>` there instead. Nothing is
forwarded and there is no per-session process.

## Where state lives

A session's state is a field of its `Session`, or derived from its log on read; a
task the session runs is spawned under its `SessionRuntime`'s `Swansong`, so ending
the session ends the task. State every session shares is a field of `Host`, and a
host-wide task or listener runs under the host's `Swansong`. Each piece of state has
one owner whose drop or shutdown is its teardown, so none needs a cleanup path of its
own. A `static` with interior mutability under `src/daemon/` would be state outside
both owners, and `the_daemon_keeps_no_state_in_statics` (`src/daemon/test/statics.rs`)
fails on one; a value computed once and never changed can be a `const`, or live
outside the daemon. Artifacts follow the pattern: their links are derived from the
log and signed with a key on `Session`, and their loopback listener runs under the
host's `Swansong` ([artifacts.md](artifacts.md)).

The `*` rewrites the conn's *path*, so the control routes match `/v1/…` exactly as
they do at a session bind's root; it does not clear the *params*. Both live in one
per-conn namespace, so a session-bind route's own `:name` must differ from the
session segment's — the segment is `:session` for that reason. A collision is
silent and reads as a malformed id rather than a routing fault: with the segment
named `:id`, `/sessions/<name>/v1/processes/1` reached the handler with `id` =
the session name, and every `/v1/processes/:id` and `/v1/notifications/:id` route
answered `400 process id must be an integer` for a well-formed ordinal.

Creation is a route on the socket handler tuple and a refusal on the web tuple:
listener topology decides where a session request — which carries the caller's
environment — is accepted, rather than a per-conn check. `POST /sessions` resolves
the body's `model`, `spotter_model`, and `triage` selections (each optional) against
the host's `InferenceRegistry` ([transport.md](transport.md)) before anything is
minted, reserves the id as a starting entry in the host's map and answers, then — on
a task of its own — establishes the environment, builds a `SessionRequest` carrying
the body and the resolved endpoints, runs `boot::prepare`, starts the runtime, and adopts it in
place of the starting entry ([control-api.md](control-api.md), "A starting session").
The `Host` is constructed with the registry its launch loaded; the sessions mutex is
not held across preparation. An MCP authorization opens a browser on the host's
machine.

The host's output is logging, not a transcript. With `RUST_LOG` unset it
announces session lifecycle — created, resumed, archived, ended (`info`) — a newer
release when its update check finds one (`info`, [update-check.md](update-check.md)),
and transport and operational failures (`warn`, `error`), and prints nothing else.
Session-scoped lines carry the session's id as their prefix (`session <id>: …`),
the shape the lifecycle announcements set; a session without an id — tests,
offline — prints unattributed.
Per-session detail — the boot report, turn-loop chatter, keepalive, model prose
— logs at `debug`, so `RUST_LOG=efference=debug` restores it; a boot failure's
detail rides the starting session's failure rather than a print. The
one direct print is the interactive MCP authorization prompt. An island host's
output is its `host.log`, which the
launcher prints only when startup fails ([operating.md](operating.md));
its lifetime is bound to the launcher by a pipe on its stdin rather than by any
connection to this socket (`src/daemon/island.rs`).

## The route set

Every route is served under the `/v1` prefix; the paths here omit it
([control-api](control-api.md) is the consumer-facing reference).

Every terminal response **halts** its conn — trillium's router does not halt on
a route match, and a web transport mounts the frontend handler after the routes,
which treats an un-halted conn as unclaimed ([web-bind.md](web-bind.md)). The
error helpers (`fail`/`respond` in `daemon/errors.rs`) halt centrally; success
paths halt per-route. The `/v1/events` validating half is the deliberate
exception: on success it must fall through to the SSE half of its own route.

**Writes:**

- `POST /turns {text}` → 202. Enqueues a user turn. Returns immediately; submission is decoupled from inference. With `?wait`, the connection is held until the daemon **settles** (the loop reaches a phase that waits on someone, or idle) and the response is the resulting `/state` snapshot instead of the 202 — synchronous "submit, read what happened" with no `/state` poll loop.
- `PUT /approvals/{call_id} {decision: approve|abort|redirect|config_edit, message?, change?, note?}` — resolves the pending gate, the call's id as a precondition: addressing anything but the pending gate is a 409 `gate_moved` whose `detail` carries the current gate (`config_edit` carries the `change`, see Approval below). With `?wait`, holds until the daemon settles again (the turn's *next* gate or completion) and returns that `/state` — so a multi-gate turn is driven as a clean approve→read loop.
- `POST /approvals/{call_id}/escalate {source?, note?}` — passes the pending gate to the operator undecided: it stays held, marked with its escalator and note (docs/permissions.md). 409 `gate_moved` as above.
- `GET`/`PATCH /loop {pace?: {speed_ms?, held?}, review_level?}` → 200, returns the resource — the operator dials, named for their state topics ([operator participation](#operator-participation--the-intervention-points), [permissions](permissions.md)): `pace` (both fields optional: slow, pause, or resume in one call; never touches the log), `review_level` (`autonomous` / `supervisor` / `spotter_human` / `human_only`), and the read-only `loop_phase`. One call can move both dials; a part-bad request moves nothing.
- `POST /loop/continue` → 202. The zero-token continue ([control-api.md](control-api.md)): sets a `continue_pending` flag and wakes the idle loop's `select!`, which re-enters the trajectory with **nothing appended** — no `User` entry, no projection change. A flag rather than a queued message, because the verb carries no payload (two presses are one continue) and because the flag is what a wake landing before the loop parks lands on. Declined `409 not_continuable` while the loop is busy, gated or asking, or when the projected wire tail means nothing to re-prompt; the check is `Session::continue_verdict`, whose answer the `verbs` topic publishes. Distinct from an empty `POST /turns`, which is reserved for keep-idling.
- `POST /loop/interrupt` — fires the interrupt notifier (kill). A verb-route because it is one: it signals the running tool or ends the generation in flight, it doesn't delete state.
- `PATCH /processes/{id} {background?, muted?, note?}` — process property writes ([process-monitor.md](process-monitor.md)): `{background: true}` on the foreground entry is the handoff (→ 202; fires the detach notifier, with the optional note delivered as a user message alongside the handoff result), `muted` is the dismiss-a-live-process toggle. `DELETE /processes/{id}` kills the process group.
- `POST /notifications {source, body, slot?}` → 202, returns `{seq}` (see [the inbox](inbox.md)): the `seq` of the notification's `Ambient` log entry, appended at arrival, so a restart keeps it. A `slot` binds the notification to a logical channel; a later notify on the same slot supersedes the old one (archive-and-replace, not edit — records are immutable). A slot notification waits in memory until a triage shows it and answers `{}`. No route withdraws a notification.

**Reads:**

- `GET /events?include=<spec>&since=<seq>` — the unified SSE control stream ([control-api](control-api.md)): one connection carrying three durability classes under a discriminated frame envelope — `log` (durable entries, `id: <seq>`, resumable with `?since=<last_seq + 1>`), `output` (lossy pre-commit deltas: `assistant_delta`, `thinking_delta`, `tool_call_start`, `tool_call_input`, `tool_output`, `process_output` — id-less, not durable; the thinking/tool-call kinds are what let a client distinguish a working turn from a hung one), and `state` (whole-topic replacements, snapshotted on connect, delivered coherently with the log).
- `GET /log?since=<seq>` — JSON array of log entries from `seq` onward. The catch-up read for a reconnecting client.
- `GET /state` — JSON snapshot of ephemeral control state: `last_seq` plus one field per state topic — `gate` (null when none is open), `notifications`, `processes` (every running tool-spawned process — structure only, foreground included, flagged rather than filtered), `usage` (last request's token counts, running totals, the absolute stamp of the last request, `context` — the boot-fetched window limit with the last request's prompt-token occupancy — and `cost` — the billed session total where the provider has a per-request accounting surface, see [transport](transport.md)), `pace` (the operator's `{speed_ms, held}` dial), `review_level` (`autonomous`/`supervisor`/`spotter_human`/`human_only`), `resolved_review_level` (the position in force: the dial's own value, except that `supervisor` with no live supervisor reads `spotter_human`), `supervisor` (the id of the session that answers this one's gates, or `null`), and `role` — the **effective** policy in force (base config + every committed config edit folded in): `source` (config path or `null`), and the full contents of `scope`, `watch_for`, `commands`, `tools`, `paths`. Effective, not boot: a live config edit (a session grant, an fs-watch file change) shows up here, so an orchestrator can read a subordinate's live perimeter (see [permissions](permissions.md)).
- `GET /state?wait=settled` — long-poll: blocks until the daemon reaches a **decision point** — the loop phase is `gated`, `asking`, `cold_parked`, or `blocked` (a triage held on the upstream, which waits on a continue — [triage.md](triage.md) — a held pace, or a config-repair park), or `idle` with nothing queued — then returns the `/state` snapshot (returns at once if already settled). The wait-without-acting verb; the same predicate backs the `?wait` flags above. The predicate is `Session::is_settled`: `idle` settles only with no queued turn about to run and no continue pending (a submitted-but-not-yet-dequeued turn still reads as busy, closing the submit race; a withdrawn one stops reading as busy at once; an agent's turn held back behind the operator's hold does not count). It decides from one snapshot, `Session::phase_and_queue` (`src/daemon/settle.rs`): the pace and queue locks are held across the phase read, and each write the predicate pairs with the phase — a claim's phase move and removal, a continue's set and take, a queued turn and its hold — is made under one of those locks. A check that overlaps a claim or a turn's end therefore reads the state on one side of it, never an idle loop with nothing queued in between; `turn_delivery` and the continue verdict read the same snapshot. Capped by a server-side timeout so a wedged loop can't hold a client forever; on timeout it returns the current state and the client re-polls.

## Transport and binding

`SessionPaths` (`src/daemon/paths.rs`) supplies absolute working and storage
roots to daemon boot. The CLI resolves these before calling `serve_host`;
relative roots are rejected rather than interpreted inside the daemon. Usage
records and the attachment watcher use the storage root, and attachment labels
are relative to the session working root when contained within it. `App::session`
receives that working root explicitly for the web file viewer. The identity topic
uses the same root. The listener's socket path is a separate input and does not
determine storage; the CLI still places it in the session directory.

The daemon binds with trillium's `bind_uds(path)`. Each session lives in its own directory — `.efference/<id>/` (the id is `control create --name <id>` or the new-session form's, or a minted word pair, [session-title.md](session-title.md)) — holding standard-named files: `log.jsonl` and (the snapshot) `config.toml` (see [permissions](permissions.md)). A hosted session has no socket of its own: it answers under `/sessions/<id>/` on its host's socket. **Hosts are found in two places**, in order: this project's `.efference/host.sock`, then the user's `~/.efference/host.sock` — what `efference --host` binds, so one host serves every project; `--host --socket <path>` binds elsewhere (a project-local host is `--socket .efference/host.sock`). A client verb resolves its target through a host alone. The TUI (`ClientTarget` in the CLI) takes `efference --name <id>` — a live host whose `GET /sessions` lists the id **with this directory as its cwd** — else the single session the live hosts list here. `control` and `mcp` (the library's `ClientExecutor`) ask the host for the `--agent`/`agent` they are given — it holds the id or it does not, from any directory; a `control` verb that takes `--agent` optionally and gets none drives the single session the host lists with this directory as its cwd, and the `mcp` mount, which has no default, requires `agent` on the calls that act on an agent. The cwd scope is what keeps a user-wide host from answering project A's plain `efference` with project B's session. The new-session form creates on the first live host; `control` and `mcp` address the first live host, else the user's, since a mount may outlive the host (`--host-socket` overrides, on both). Liveness means a successful connect, not a file's existence, so a stale dir from a dead process doesn't shadow a running one. A name no host lists, or zero or several candidates with no name, is an error before any UI opens; the TUI also opens its event stream before taking the terminal, so a dead target is a printed error rather than an empty screen.

**The socket and the state beside it are owner-only.** The bind mints `host.sock` under
the process umask, so boot tightens it to `0700` after binding, and the directory it sits
in and the state files beside it — `state.db`, `config.toml`, and the names earlier
releases wrote — to `0700`/`0600` (`src/daemon/permissions.rs`,
[state-store.md](state-store.md)). Connecting to a socket needs write permission, so
owner-only is exactly the local accounts that may reach the channel; a mode stricter
than the target is kept, and an absent file is skipped.

**Creation mints a session dir; it never reuses one.** The dir is created with `create_dir` (not `create_dir_all`) on the leaf, so an existing `--name` is refused with an error naming the dir and the log inside it, and two creations racing the same name cannot both proceed — the loser is refused
before it mints anything, so its rollback has nothing of the winner's to remove. A session does not outlive its process: `Log::create` opens the log for append with an empty in-memory `entries`, so a second run over the same file would restart `seq` at 0 and leave a duplicated seq space that every offline reader (`view`, `wire`, the TUI fold) reads as one sequence. The guard also runs before the socket is unlinked, so a second creation named `X` cannot take the socket away from a running session. Reopening an existing session is a separate path that never mints: `PATCH /sessions/:id` ([resumption.md](resumption.md)).

Clients dial via `UnixClientConfig`, which routes requests to a fixed path — the URL host is a placeholder; only path and headers matter.

**Request bodies pin `charset=utf-8` explicitly.** `request_body_string()` — what the `/turns` handler uses — decodes according to the request's `Content-Type` charset and defaults *away* from UTF-8 when none is given, so an unlabeled body silently mojibakes any non-ASCII text (em-dashes, smart quotes, accents) on its way into the log. Every client `POST` sets the charset; this is a correctness requirement on the client side, not a nicety.

The cross-host TCP+TLS case (see [transport](transport.md)) is the same code path: `bind_tcp` or `bind_env` instead of `bind_uds`, a configuration switch rather than a new abstraction.

The client side has the matching interface: `Client::new(socket)` is `Client::with_connector(UnixClientConfig::new(socket))`, and every verb below the constructor is transport-agnostic and prefix-agnostic — `Client::at(socket, prefix)` re-bases the `/v1` verbs under a host's `/sessions/<id>` ([control-api.md](control-api.md)) without touching them. `trillium_client::Client` type-erases its connector, so this costs no type parameter. Two consumers: a non-UDS binding differs from the UDS one only in which connector it hands over, and the daemon's own tests drive the real router over `trillium_testing`'s in-memory transport — no socket bound, so no fixed path to collide on and no exposure to Darwin's 104-byte `SUN_LEN` limit. The one wrinkle worth knowing: shared state reaches handlers through the server-global TypeSet, which a bind populates with `with_shared_state` and a test populates with an `Init` handler run by `trillium_testing::init` — the bare `connector()` never runs startup, so without that every route answers `500 missing shared state`.

## The Frontend trait

The agent loop and the log know only the `Frontend` trait. Swapping the client — a different TUI, a web client — touches only this boundary, never the log or projection.

```rust
trait Frontend {
    async fn read_user_turn(&mut self) -> Option<String>;
    async fn approve(&mut self, call: &ToolCall, gate: GateOptions<'_>) -> Approval;
    async fn interrupted(&mut self) -> Interrupt;
}
```

`read_user_turn` returns the next user turn, or `None` to end the loop. `approve` is a gate on a pending tool call, not a general line-edit surface — it has its own four-valued return (approve / abort / redirect / config_edit) and is conceptually separate from turn input. `GateOptions` carries what the gate shows beside the call, each part optional: the leaf action an `agents__decide` authorizes and the grant the gate offers ([permissions.md](permissions.md)); `register_pending` takes the same value. `interrupted` resolves when the user signals interrupt while a tool is running, carrying the intent — `Kill` (abort the tool, process group killed) or `Detach` (hand the live child to the background process monitor, see [process-monitor.md](process-monitor.md)).

The loop is pull-shaped and stays that way. It blocks at `read_user_turn()` and `approve()`, and these map cleanly onto request/response: the pull resolves when a client pushes. `RemoteFrontend` implements all three against `Session` — `read_user_turn` awaits the turn inbox, `approve` registers a gate and awaits its oneshot, `interrupted` selects over the kill and detach notifiers.

## Approval: four-valued, no always-allow

`Approval` is `Approve | Abort | Redirect(String) | ConfigEdit { change, note }`.

- **Empty line (or `PUT /approvals/{id}` with `decision: approve`)** — run the call, no policy change.
- **Any other text** — redirect. The call is rejected; the text is sent to the model as the next user turn, steering it without ending the turn.
- **C-c (or `POST /loop/interrupt`)** — abort. The call is rejected; the turn unwinds. Fired at a gate this is the un-run abort; fired at a *running* call it is [the interrupt](#the-interrupt), which withholds the result instead.
- **`decision: config_edit`** — the orchestrator edits the config (docs/permissions.md): one `change` (`command`/`path`/`tool`/`scope`). The loop writes it to the session's config file (a format-preserving `DocumentMut` edit — a failed write fails the grant) and appends a `Payload::ConfigEdit` (folding it into the effective config by replace-at-key), then **re-classifies this call against the new policy** — it runs only if the changed rule now clears it, otherwise it stays gated as a redirect. The human changes the *rule*; the mechanism, not the gesture, authorizes the *call*.

There is still no always-allow mode, and `ConfigEdit` is not one: it is a **specific, logged, prunable** change (a named command-prefix / path / tool / the scope), never a blanket "stop asking." A gate offers at most one such change, chosen from why the call held (`gate.grant`, [permissions.md](permissions.md)). A re-classified call is re-checked against the new policy, so nothing rides along unchecked. The gate exists so every command is seen before it runs — a guardrail, not a security posture.

**Every tool call that doesn't run still produces a `ToolResult { is_error: true, wire: Raw }`.** This is an API hard requirement: every `tool_use` in the assistant turn must have a paired `tool_result` in the next user turn. Two flavors exist: `aborted()` (from C-c) and `rejected()` (from a redirect). A call that *did* run but was interrupted takes a third shape — `withheld()`, which keeps `raw` and projects a line saying who interrupted it (`WireState::Interrupted`); see [the interrupt](#the-interrupt). On a redirect, a real `User` turn with the human's steering text is appended after the rejection `tool_result`. A `tool_result` can't be followed by bare user text as a separate message, so `project` **merges** a `User` entry that directly follows a turn's `tool_result`s into that same user message as `[tool_result…, text]` (see [log-and-projection.md](log-and-projection.md)) — the model reads the rejection and the direction together, in the human's own words. This is the identical merge mid-trajectory delivery of a queued turn relies on: an injected `Payload::User` lands right after the results and projects into one role-alternating user message.

## The loop shape

```
(inner loop, per step; pace + drain skipped on the first step of a trajectory)
pace_at_juncture (interruptible sleep) → drain queued user turns → project → complete → append Assistant →
  { approve     → run → append ToolResult
  | config_edit → append ConfigEdit (fold policy; notify if session-visible) → re-classify → {approve|redirect}
  | abort       → append ToolResult(is_error)
  | redirect    → append ToolResult(is_error) + append User turn }
→ repeat
```

The between-step juncture — top of the inner loop, after a step's results land, before the next model call — is where the participation affordances act (below): the pace sleep (`pace_at_juncture`), the mid-trajectory turn drain (`drain_queued_turns`), and the soft-Esc hold. Both are skipped on the first step so a fresh user turn / pinned-event wake gets a prompt first response; pacing is a *between-turns* delay.

## Operator participation — the intervention points

One intent sits behind the pace dial, mid-task delivery of queued turns, graduated Esc, and the review-level gears: **"wait and/or slow down — I have input and want to participate."** The underlying friction is that the **operator runs in wall-clock time, the agent runs in turns.** Turns fly past in wall time, so operator input arrives at an arbitrary moment and has to be *quantized* onto the loop.

Four points where an operator intent can take effect:

1. **Mid-tool** — a bash process is running. Only a *preemptive* intervention reaches here: kill the process. Nothing else can.
2. **Juncture** — a tool's results just landed, before the next model call (top of the inner-loop iteration). The natural place to deliver input and to hold.
3. **Turn boundary** — the assistant finished its calls; about to loop or go idle.
4. **Idle** — the outer `select!`, between trajectories.

These are one continuum. Pace moves *continuously* along the between-turns part of it (points 2/3); Esc is the *discrete* step that also reaches the one point pace can't (point 1). That is what "Esc is graduated by intervention point, not a menu" means — the rungs answer *"is my input for the **next** step, or about **this** one?"*

Three commitments hold across all of them:

- **User messages are canonical, never triageable.** A user turn lands as `Payload::User`, undismissable — *not* through the inbox/triage path. Triageability is the inbox's defining feature (pin/dismiss/leave); routing a user turn through it would let the model *dismiss the operator*. Wrong by construction.
- **Pace/hold is operator-view state, never logged — so it is structurally invisible to the agent.** It lives in daemon/frontend state and never becomes a log entry, so it cannot reach `project(log)`; and even the timing that *is* logged (each entry's `at`) is never projected. Two independent guarantees, hence no "Paused/Resumed" trace: there is nowhere in the projection for one to appear.
- **Resume = drop the `held` overlay; return to the retained speed.** Hold is an overlay on the dial, not a position of it. Two triggers both resume — silent release, and the operator's own submit (the operator saying something always resumes); there is no "submit but stay paused." An agent's message does not resume a session its operator paused: it waits behind the hold, and goes to the loop on the release. Dials are freely settable *during* a hold and outlive it, so a mode or speed change made while paused recolors what resume does.

A redirect does not break the loop. The model receives the rejection and the steering text and responds — the loop continues from `project` with the new state. The approval gate sits in exactly the right spot for the eventual concurrent TUI, which will have it in this same position.

## The interrupt

The gate answers a call that hasn't run. The interrupt answers one that is already running, or already past — the case where the operator has seen enough and wants the turn to stop *and* wants what it produced kept out of the context. A `read` of the wrong file is the ordinary example: it was cleared by policy, so no gate ever offered a stop, and its output is about to become several thousand tokens of prefix that the model will reason from for the rest of the session.

Nothing here can suspend a tool. The MCP providers are synchronous (`src/tools.rs`), so a call that has started will finish; what the interrupt controls is not whether it runs but **whether its output is projected**. `Session::take_interrupt` consumes the latch at the two junctures around a call:

- **before a call runs** — the operator interrupted while the model was generating, so the call doesn't run and the turn ends. Its `tool_use` gets the ordinary un-run `aborted()` stand-in.
- **where the result lands** — a bash command's process group is killed; a synchronous tool completed and its effects stand. The result is appended with `raw` intact and `wire: WireState::Interrupted(Interruption { by, after_ms, killed })`, and the turn ends. This is taken *before* the triage, so the withheld output is never triaged at, never triaged, and costs no round-trip — a fork that read it first would defeat the point on its way to the same place.

- **mid-generation** — no tool is running or pending; the model is streaming. `Client::complete` races its read against the same notifier (`Client::set_abort`, armed once per request before anything is sent and kept across its attempts, so a press that lands after the request reached the upstream but before its stream opened is not lost — `notify_waiters` stores no permit) and ends the read: the stream is dropped, which is what frees a hung socket, and the partial comes back marked `FailureClass::Aborted`. The loop's model request also reads the latch once that notifier is armed (`Client::with_interrupt_latch`). A press sets the latch before it notifies, and `notify_waiters` wakes every waiter created before it, so a press after the loop's last juncture either wakes the armed read or set the latch the request then reads; one that reached the request before the arm stops it unsent, with no stage recorded, and it ends as a read the interrupt ended with nothing streamed. Only that request reads the latch. The trajectory's start and every juncture consume the presses before it, so a set latch there is a press since; the other requests on these clients — the keepalive pings, the idle triage, title peeks, compactions — may run with an earlier press still latched, and answer only to a press during them. The step's result triage reads only the notifier too: a press just before it is taken at the juncture right after it. The turn loop commits it as the record-only entry an aborted attempt writes, counts it against no retry budget, consumes the latch, and returns to idle — unlike a killed tool there is no result to continue from, and a queued submission lands at once. The own-triage client reads against the notifier too: an interrupted peek withholds its verdict (the step's results are logged under the failed verdict), and the loop consumes the latch right after the gate, ending the turn there rather than letting it ride into the next completion and stop that one's first call. The spotter does not — an interrupt during a gate is the tool's kill.

The latch is one-shot (consumed exactly once per press) and is cleared again when a fresh user turn starts, so an interrupt fired at idle can't stop the next turn's first call.

The wire states the fact and nothing more: `[interrupted by the operator after 5s: the command was killed, and its output was withheld]` (`project::render_interruption`). `by` is read from the recorded `Telemetry::Interrupt`, which the route appends before it sets the latch, so the loop reading the latch reads the record that set it; `after_ms` is the call's own elapsed time; `killed` is bash's kill marker (`bash::KILLED`) at the end of `raw`. An interrupt during the triage, after the call finished, is recorded the same way with no `after_ms`, and reads `[interrupted by … before its result was read]` (`HoldOutcome::interrupted`). The result has to exist at all because an assistant `tool_use` with no paired `tool_result` is a request the API rejects, and a next message merges into the same user message (the same merge a redirect uses), so a person who interrupts to say why is read right after the line: `[tool_result, "read the test instead"]`. The line does not guess why. It was once a bare `[elided]` placeholder, on the premise that whoever interrupted would say why next. An MCP driver or a supervising agent usually does not, and the model then could not tell its call had been stopped (journal/2026-10-17-session-control-live-test.md).

Withheld from the model is not withheld from the operator: `raw` is in the log, so the TUI can still show it and the verdict is reversible like any other (see [triage.md](triage.md)). Older logs carry `WireState::Elided` here, which projects as it always did.

An interrupt ends the turn. When a turn is already queued, the loop passes straight through idle into it, and no `settle` push goes out for that idle (`Session::going_idle`): the session is not waiting on anyone.

## The CLI

A hosted session opens the log file and runs the agent loop with a `RemoteFrontend`. The loop parks at `read_user_turn` until the first `POST /turns` arrives.

The TUI ([tui.md](tui.md)), bare `efference`, is the interactive client: it tails `/events` and renders, and its composer either resolves the pending gate (empty approves, any text redirects) or sends a new turn.

Scriptable verbs share one `client.rs` over the socket, grouped under `efference control`, whose agent verbs `efference mcp` serves from the same definitions ([operating.md](operating.md)): `efference control message --agent <id> <words>…`, `efference control decide --agent <id> <seq> allow|deny [--message <msg>]`, `efference control status`, `efference control settle`, `efference control interrupt --agent <id>`, `efference control pace [--speed-ms N] [--hold | --resume]`, `efference control transcript --agent <id> [--audit] [--since N | --tail N]`, `efference control entries <seq>…`, `efference control state`, and against the host root, `efference control create [--name id] [--cwd dir] …`, `efference control archive <id>`. `settle` blocks until the daemon settles and prints the resulting `/state`, so a script drives the daemon synchronously — *act, read state, act* — instead of polling in a wall-clock loop. No signal is involved; a script gets the same gate an operator does.
