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

**Turn inbox** — an unbounded MPSC channel. `POST /turn` enqueues a string; `RemoteFrontend::read_user_turn()` awaits the next pop. This also seeds turn 1: an idle daemon parks at `read_user_turn` waiting for the first `POST /turn`, which retired argv-seeding and the "backgrounded run can't get its opening prompt" problem.

**Pending-approval registry** — a `HashMap<CallId, oneshot::Sender<Approval>>`. When the loop reaches a tool call requiring approval, it calls `Shared::register_pending`, which creates a oneshot channel, stores the sender keyed by the call's `CallId` (the same id already on the wire — no new id scheme), and returns the receiver. The loop blocks on that receiver. `POST /approvals/{call_id}` looks up the sender and fulfills it.

**Interrupt `Notify`** — `POST /interrupt` fires this; `RemoteFrontend::interrupted()` resolves when it's notified. The loop races this against a running tool (the SIGINT equivalent, killing the tool's process group).

**Append `Notify`** — pinged by every `Shared::append` call; the `GET /events` tail arms a `Notified` before each catch-up read so an append landing between the read and the await cannot be lost (the lost-wakeup-safe pattern).

**Broadcast for stream deltas** (`broadcast::Sender<StreamDelta>`) — carries ephemeral pre-commit output fragments to `GET /stream` subscribers. Lossy by design: a consumer that lags past the ring drops the oldest deltas rather than stalling the loop. The committed entry on `/events` is the authoritative copy, so the stream is interface candy with the durable log as its backstop.

## The verb set

**Writes:**

- `POST /turn {text}` → 202. Enqueues a user turn. Returns immediately; submission is decoupled from inference.
- `POST /approvals/{call_id} {decision: approve|abort|redirect, message?}` — resolves a pending gate by the tool call's id.
- `POST /interrupt` — fires the interrupt notifier.
- `POST /notify {source, body, slot?, replaces?}` → 202, returns `{id}`. Enqueues an ambient notification into the ephemeral inbox (see [the inbox](inbox.md)). A `slot` binds the notification to a logical channel; a later notify on the same slot supersedes the old one (archive-and-replace, not edit — records are immutable). `replaces` supersedes a specific pending id by its queue-local id.
- `POST /notifications/:id/dismiss` — removes a pending notification from the ephemeral inbox before it reaches a triage gate.

**Reads:**

- `GET /events?since=<seq>` — durable SSE tail of the log. Each event: `event: <kind>` (user/assistant/tool_result/mark/triage/ambient), `id: <seq>`, `data: <LogEntry JSON>`. A reconnect resumes with `?since=<last_seq + 1>`.
- `GET /stream` — lossy SSE stream of ephemeral output deltas. Event types: `assistant_delta` (prose fragment), `tool_output` (tagged by `call_id`), `bg_output` (tagged by background process handle). No `id` field — these carry no `seq` and are not durable.
- `GET /log?since=<seq>` — JSON array of log entries from `seq` onward. The catch-up read for a reconnecting client.
- `GET /state` — JSON snapshot of ephemeral control state: `last_seq`, `pending_approval` (if any), `pending_notifications`, `processes` (live background process views), and `usage` (last request's token counts, running totals, seconds since last request).

## Transport and binding

The daemon binds with trillium's `bind_uds(path)`, placing the socket at `.efference/control.sock` (overridable with `--socket`). Clients dial via `UnixClientConfig`, which routes requests to a fixed path — the URL host is a placeholder; only path and headers matter.

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

`read_user_turn` returns the next user turn, or `None` to end the loop. `approve` is a gate on a pending tool call, not a general line-edit surface — it has its own three-valued return and is conceptually separate from turn input. `interrupted` resolves when the user signals interrupt while a tool is running.

The loop is pull-shaped and stays that way. It blocks at `read_user_turn()` and `approve()`, and these map cleanly onto request/response: the pull resolves when a client pushes. `RemoteFrontend` implements all three against `Shared` — `read_user_turn` awaits the turn inbox, `approve` registers a gate and awaits its oneshot, `interrupted` awaits the interrupt notifier.

## Approval: three-valued, no always-allow

`Approval` is `Approve | Abort | Redirect(String)`.

- **Empty line (or `POST /approvals/{id}` with `decision: approve`)** — run the call.
- **Any other text** — redirect. The call is rejected; the text is sent to the model as the next user turn, steering it without ending the turn.
- **C-c (or `POST /interrupt`)** — abort. The call is rejected; the turn unwinds.

There is no always-allow mode. The gate exists so every command is seen before it runs. This is a guardrail, not a security posture: the risk of a catastrophic command is low, but the gate stops it before it runs and costs nothing for the normal case.

**Every tool call that doesn't run still produces a `ToolResult { is_error: true, wire: Raw }`.** This is an API hard requirement: every `tool_use` in the assistant turn must have a paired `tool_result` in the next user turn. Two flavors exist: `aborted()` (from C-c) and `rejected()` (from a redirect). On a redirect, a real `User` turn with the human's steering text is appended after the rejection `tool_result`. The API requires that a `tool_result` response can't be followed by bare user text without being in the same user message, so the two consecutive user-role payloads merge server-side into one turn — the model reads the rejection and the direction together, in the human's own words.

## The loop shape

```
project → complete → append Assistant →
  { approve → run → append ToolResult
  | abort   → append ToolResult(is_error)
  | redirect → append ToolResult(is_error) + append User turn }
→ repeat
```

A redirect does not break the loop. The model receives the rejection and the steering text and responds — the loop continues from `project` with the new state. The approval gate sits in exactly the right spot for the eventual concurrent TUI, which will have it in this same position.

## The CLI

`efference serve` opens the log file, binds the control socket, and runs the agent loop with a `RemoteFrontend`. The loop parks at `read_user_turn` until the first `POST /turn` arrives.

`efference attach` is the interactive client: a background task tails `/events` and `/stream` and renders, while the foreground reads stdin and per line either resolves the pending gate (Enter approves, `.abort` aborts, any other text redirects) or sends a new turn. It is spartan by design — the full TUI (see [the TUI](tui.md)) is a later client that co-evolves the API.

Scriptable verbs share one `client.rs` over the socket: `efference turn <text>`, `efference approve [call_id] [--abort | --redirect <msg>]`, `efference interrupt`, `efference log [--since N]`, `efference state`. A bare `efference approve` defaults to the one pending gate. These replaced the earlier SIGUSR1 hack.
