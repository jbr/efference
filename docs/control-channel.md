# Control channel & the Frontend trait

## The singular input source

The HTTP-over-Unix-socket interface is not merely equivalent to other input paths — it is the only input path. There is no privileged in-process route. The loop's sole `Frontend` implementation is `RemoteFrontend`, which reads from shared daemon state populated exclusively by HTTP handlers. The human using the TUI client, an automated driver (such as Claude Code), and a subagent are all HTTP clients of the same socket. The one added hop — the approval round-trip — is microseconds against inference latency and the Anthropic round-trip, so it costs nothing in practice.

This is the interface-parity principle carried to its conclusion: the TUI is not a special client with privileged access, it is one client among peers. Two payoffs follow directly. First, any driver can exercise the full harness without a human hand-off — the control channel is the same surface regardless of who is on the other end. Second, subagents fall out for free. The subagent boundary *is* the client boundary: an efference agent's tool can spawn and drive another efference daemon over the same socket interface. Agents addressing each other is just one agent injecting an `Ambient` event into another's log, triaged at its head (see [the inbox](inbox.md)) — no separate messaging system is needed.

## The log is already the event store

The hardest control-channel problem — SSE has no replay, so a reconnecting client drops events — dissolves because of the substrate's design (see [substrate](substrate.md)). The append-only log with monotonic `seq` *is* the event store.

`GET /events` tails the log as a Server-Sent Event stream. Each event carries `id: <seq>` and `data: <serialized LogEntry>` — the same JSON `GET /log` returns, so the tail and the catch-up read are byte-identical per entry. A reconnecting or late-joining client replays history with `GET /log?since=<seq>` and then resumes the SSE tail, deduplicating by `seq`. The slow-consumer case is also safe: because the log is the buffer and each `GET /events` connection reads at its own pace, a slow client never drops events (unlike a broadcast channel).

Ephemeral control state — a pending approval, current status, live process tails — is not a log payload. The log records the *result* of a gate (approved and ran, or rejected), not the in-flight gate itself. A connecting client therefore does three things in sequence: `GET /state` to fetch current ephemeral state (last `seq`, any pending approval, usage telemetry, running background processes), `GET /log?since=<seq>` to catch up on history, then attach to `GET /events` and deduplicate by `seq`.

## Shared: the loop ↔ handlers seam

`Shared` is the bridge between the agent loop task and the HTTP handlers. Both hold it as `Arc<Shared>`. The daemon registers it as trillium's server-global shared state; handlers retrieve it per-connection. Its contents:

**`Arc<Mutex<Log>>`** — the persisted, append-only log (see [substrate](substrate.md)). The loop appends under this lock via `Shared::append`, which also pings the append notifier so SSE tailers wake. The lock is never held across an `await`; code that needs to call the model on a snapshot calls `Shared::snapshot()` to clone the entries first.

**Turn inbox** — an unbounded MPSC channel. `POST /turn` enqueues a string; `RemoteFrontend::read_user_turn()` awaits the next pop. This also seeds turn 1: an idle daemon parks at `read_user_turn` waiting for the first `POST /turn`, which retired argv-seeding and the "backgrounded run can't get its opening prompt" problem. A turn submitted while the loop is *mid-trajectory* doesn't wait for the whole trajectory to end: `RemoteFrontend::drain_injected_turns()` non-blockingly pulls the queued turns at the next juncture (top of the inner loop, after the last tool's results land) and the loop appends each as a canonical `Payload::User` — the seam-drain (backlog/participation/README.md). A user turn is undismissable; it never routes through the inbox/triage path.

**Pending-approval registry** — a `HashMap<CallId, oneshot::Sender<Approval>>`. When the loop reaches a tool call requiring approval, it calls `Shared::register_pending`, which creates a oneshot channel, stores the sender keyed by the call's `CallId` (the same id already on the wire — no new id scheme), and returns the receiver. The loop blocks on that receiver. `POST /approvals/{call_id}` looks up the sender and fulfills it.

**Interrupt `Notify`** — `POST /interrupt` fires this; `RemoteFrontend::interrupted()` resolves when it's notified. The loop races this against a running tool (the SIGINT equivalent, killing the tool's process group).

**Pace state** — `Mutex<Pace>` (`{ speed: Duration, held: bool }`) plus a `pace_interrupt` `Notify` (backlog/participation/README.md). The operator's between-turns wall-clock dial: `effective_sleep = held ? ∞ : speed`, `held` an overlay that retains `speed` underneath (resume returns to it, never to zero). `Shared::pace_at_seam` applies an interruptible sleep at the seam; `set_pace` (via `POST /pace`) moves the dials, and a **release** or a **submit** fires `pace_interrupt` so the wait ends immediately. It is **operator-view only — never a log entry, never projected**, so a held or slowed loop is byte-identical on the wire to a fast one (that, plus timestamps-never-projected, is the whole invisibility guarantee: loud to the operator's TUI, silent to the agent).

**Append `Notify`** — pinged by every `Shared::append` call; the `GET /events` tail arms a `Notified` before each catch-up read so an append landing between the read and the await cannot be lost (the lost-wakeup-safe pattern).

**Broadcast for stream deltas** (`broadcast::Sender<StreamDelta>`) — carries ephemeral pre-commit output fragments to `GET /stream` subscribers. Lossy by design: a consumer that lags past the ring drops the oldest deltas rather than stalling the loop. The committed entry on `/events` is the authoritative copy, so the stream is interface candy with the durable log as its backstop.

## The verb set

**Writes:**

- `POST /turn {text}` → 202. Enqueues a user turn. Returns immediately; submission is decoupled from inference. With `?wait`, the connection is held until the daemon **settles** (the turn reaches a gate or completes) and the response is the resulting `/state` snapshot instead of the 202 — synchronous "submit, read what happened" with no `/state` poll loop.
- `POST /approvals/{call_id} {decision: approve|abort|redirect|config_edit, message?, change?, lifetime?, note?}` — resolves a pending gate by the tool call's id (`config_edit` carries the `change` + lifetime, see Approval below). With `?wait`, holds until the daemon settles again (the turn's *next* gate or completion) and returns that `/state` — so a multi-gate turn is driven as a clean approve→read loop.
- `POST /interrupt` — fires the interrupt notifier.
- `POST /pace {speed_ms?, held?}` → 200, returns the resulting `{speed_ms, held}`. Sets the operator's pace dial and/or hold overlay (backlog/participation/README.md). Both optional: slow, pause, or resume in one call. Reflected in `GET /state`'s `pace`; never touches the log.
- `POST /review-level {level}` → 200, returns the resulting `{review_level}`. Sets the review-level dial — `autonomous` / `spotter_human` / `human_only` — deciding who reviews a gated call (see [permissions](permissions.md)). Runtime-mutable; reflected in `GET /state`'s `review_level`.
- `POST /notify {source, body, slot?, replaces?}` → 202, returns `{id}`. Enqueues an ambient notification into the ephemeral inbox (see [the inbox](inbox.md)). A `slot` binds the notification to a logical channel; a later notify on the same slot supersedes the old one (archive-and-replace, not edit — records are immutable). `replaces` supersedes a specific pending id by its queue-local id.
- `POST /notifications/:id/dismiss` — removes a pending notification from the ephemeral inbox before it reaches a triage gate.

**Reads:**

- `GET /events?since=<seq>` — durable SSE tail of the log. Each event: `event: <kind>` (user/assistant/tool_result/mark/triage/ambient), `id: <seq>`, `data: <LogEntry JSON>`. A reconnect resumes with `?since=<last_seq + 1>`.
- `GET /stream` — lossy SSE stream of ephemeral output deltas. Event types: `assistant_delta` (prose fragment), `tool_output` (tagged by `call_id`), `bg_output` (tagged by background process handle). No `id` field — these carry no `seq` and are not durable.
- `GET /log?since=<seq>` — JSON array of log entries from `seq` onward. The catch-up read for a reconnecting client.
- `GET /state` — JSON snapshot of ephemeral control state: `last_seq`, `pending_approval` (if any), `pending_notifications`, `processes` (live background process views), `usage` (last request's token counts, running totals, seconds since last request), `pace` (the operator's `{speed_ms, held}` dial), `review_level` (`autonomous`/`spotter_human`/`human_only`, with `autonomous` also mirrored as a derived bool), and `role` — the **effective** policy in force (base config + every committed config edit folded in): `source` (config path or `null`), and the full contents of `scope`, `watch_for`, `commands`, `tools`, `paths`. Effective, not boot: a live config edit (a session grant, an fs-watch file change) shows up here, so an orchestrator can read a subordinate's live perimeter (see [permissions](permissions.md)).
- `GET /settle` — long-poll: blocks until the daemon reaches a **decision point** — a gate is pending, or the loop is idle (parked with nothing queued) — then returns the `/state` snapshot (returns at once if already settled). The wait-without-acting verb; the same predicate backs the `?wait` flags above. The predicate is `pending_approval.is_some() || (!running && queued == 0)`, where `running`/`queued` track the loop's turn lifecycle (a submitted-but-not-yet-dequeued turn still reads as busy, closing the submit race). Capped by a server-side timeout so a wedged loop can't hold a client forever; on timeout it returns the current state and the client re-polls.

## Transport and binding

The daemon binds with trillium's `bind_uds(path)`. Each session lives in its own directory — `.efference/<id>/` (the id is `serve --name <id>`, or a generated timestamp) — holding standard-named files: `control.sock`, `log.jsonl`, and (the snapshot) `config.toml` (see [permissions](permissions.md), backlog/permissions/session-dir-and-config-as-reviewed-file.md). A client verb finds the socket by `--name`, or by scanning the session dirs for the single **live** (connectable) one — liveness, not mere existence, so a stale dir from a dead daemon doesn't shadow the running one; `--socket <path>` is an explicit override. Clients dial via `UnixClientConfig`, which routes requests to a fixed path — the URL host is a placeholder; only path and headers matter.

The cross-host TCP+TLS case (see [transport](transport.md)) is the same code path: `bind_tcp` or `bind_env` instead of `bind_uds`, a configuration switch rather than a new abstraction.

## The Frontend trait

The agent loop and the log know only the `Frontend` trait. Swapping the underlying line-editor or replacing the spartan attach client with a full TUI touches only this boundary, never the substrate.

```rust
trait Frontend {
    async fn read_user_turn(&mut self) -> Option<String>;
    async fn approve(&mut self, call: &ToolCall) -> Approval;
    async fn interrupted(&mut self);
}
```

`read_user_turn` returns the next user turn, or `None` to end the loop. `approve` is a gate on a pending tool call, not a general line-edit surface — it has its own four-valued return (approve / abort / redirect / config_edit) and is conceptually separate from turn input. `interrupted` resolves when the user signals interrupt while a tool is running.

The loop is pull-shaped and stays that way. It blocks at `read_user_turn()` and `approve()`, and these map cleanly onto request/response: the pull resolves when a client pushes. `RemoteFrontend` implements all three against `Shared` — `read_user_turn` awaits the turn inbox, `approve` registers a gate and awaits its oneshot, `interrupted` awaits the interrupt notifier.

## Approval: four-valued, no always-allow

`Approval` is `Approve | Abort | Redirect(String) | ConfigEdit { change, lifetime, note }`.

- **Empty line (or `POST /approvals/{id}` with `decision: approve`)** — run the call, no policy change.
- **Any other text** — redirect. The call is rejected; the text is sent to the model as the next user turn, steering it without ending the turn.
- **C-c (or `POST /interrupt`)** — abort. The call is rejected; the turn unwinds.
- **`decision: config_edit`** — the orchestrator edits the config (backlog/permissions/config-edit.md): one `change` (`command`/`path`/`tool`/`scope`) with a `session` or `persist` `lifetime`. The loop appends a `Payload::ConfigEdit` (folding it into the effective config by replace-at-key), optionally persists it to the TOML file (a format-preserving `DocumentMut` edit), then **re-classifies this call against the new policy** — it runs only if the changed rule now clears it, otherwise it stays gated as a redirect. The human changes the *rule*; the mechanism, not the gesture, authorizes the *call*.

There is still no always-allow mode, and `ConfigEdit` is not one: it is a **specific, logged, prunable** change (a named command-prefix / path / tool / the scope), never a blanket "stop asking." A re-classified call is re-checked against the new policy, so nothing rides along unchecked. The gate exists so every command is seen before it runs — a guardrail, not a security posture.

**Every tool call that doesn't run still produces a `ToolResult { is_error: true, wire: Raw }`.** This is an API hard requirement: every `tool_use` in the assistant turn must have a paired `tool_result` in the next user turn. Two flavors exist: `aborted()` (from C-c) and `rejected()` (from a redirect). On a redirect, a real `User` turn with the human's steering text is appended after the rejection `tool_result`. A `tool_result` can't be followed by bare user text as a separate message, so `project` **merges** a `User` entry that directly follows a turn's `tool_result`s into that same user message as `[tool_result…, text]` (see [substrate](substrate.md)) — the model reads the rejection and the direction together, in the human's own words. This is the identical merge the mid-trajectory seam-drain relies on: an injected `Payload::User` lands right after the results and projects into one role-alternating user message.

## The loop shape

```
(inner loop, per step; pace + drain skipped on the first step of a trajectory)
pace_at_seam (interruptible sleep) → drain injected user turns → project → complete → append Assistant →
  { approve     → run → append ToolResult
  | config_edit → append ConfigEdit (fold policy; notify if session-visible) → re-classify → {approve|redirect}
  | abort       → append ToolResult(is_error)
  | redirect    → append ToolResult(is_error) + append User turn }
→ repeat
```

The seam — top of the inner loop, after a step's results land, before the next model call — is where three participation affordances act (backlog/participation/): the pace sleep (`pace_at_seam`), the mid-trajectory turn drain (`drain_injected_turns`), and (future) the soft-Esc hold. Both are skipped on the first step so a fresh user turn / escalated event gets a prompt first response; pacing is a *between-turns* delay.

A redirect does not break the loop. The model receives the rejection and the steering text and responds — the loop continues from `project` with the new state. The approval gate sits in exactly the right spot for the eventual concurrent TUI, which will have it in this same position.

## The CLI

`efference serve` opens the log file, binds the control socket, and runs the agent loop with a `RemoteFrontend`. The loop parks at `read_user_turn` until the first `POST /turn` arrives.

`efference attach` is the interactive client: a background task tails `/events` and `/stream` and renders, while the foreground reads stdin and per line either resolves the pending gate (Enter approves, `.abort` aborts, any other text redirects) or sends a new turn. It is spartan by design — the full TUI (see [the TUI](tui.md)) is a later client that co-evolves the API.

Scriptable verbs share one `client.rs` over the socket: `efference turn <text> [--wait]`, `efference approve [call_id] [--abort | --redirect <msg>] [--wait]`, `efference settle`, `efference interrupt`, `efference pace [--speed-ms N] [--hold | --resume]`, `efference log [--since N]`, `efference state`. A bare `efference approve` defaults to the one pending gate. `--wait` (and the standalone `settle`) block until the daemon settles and print the resulting `/state`, so a script drives the daemon synchronously — *act, read state, act* — instead of polling in a wall-clock loop. These replaced the earlier SIGUSR1 hack.
