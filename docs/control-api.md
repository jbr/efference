# Control API reference

> Consumer-facing reference for the daemon's control interface — written for
> someone building a *client* (the TUI, the CLI verbs, a future web/iOS/desktop
> app), not for someone changing the daemon. For the internal rationale and the
> `Frontend` boundary, see [control-channel.md](control-channel.md); for the
> streaming/transport internals see [transport.md](transport.md).
>
> **Status:** unversioned and unstable. There is no `/vN` prefix and no
> compatibility promise yet. This documents what the daemon serves *today* so we
> can critique and harden it before more clients commit to it.

## Transport & addressing

The daemon speaks **HTTP/1.1 over a Unix domain socket**. Each session owns a
directory `.efference/<id>/` (the id is `serve --name <id>`, or a generated
timestamp) holding `control.sock`, `log.jsonl`, and `config.toml`.

A client finds the socket by:

1. `--socket <path>` — explicit override, or
2. `--name <id>` — resolve `.efference/<id>/control.sock`, or
3. **liveness scan** — pick the single *connectable* socket among the session
   dirs (a stale dir from a dead daemon is skipped — liveness, not existence).

Requests dial the fixed socket path; the request URL's host is a placeholder
(`http://localhost/`) supplying only the `Host` header. Path and headers are all
that matter.

The cross-host case (TCP + TLS) is the same handler set behind `bind_tcp` /
`bind_env` instead of `bind_uds` — a configuration switch, not a different API.
It is **not** wired as a default and carries **no authentication model** today
(on UDS, filesystem permissions are the only gate).

Content type is `application/json` for all request and response bodies except
the two SSE streams (`text/event-stream`).

## Connection lifecycle (how a client catches up and stays live)

Because SSE has no built-in replay but the **append-only log is itself the event
store**, a connecting client reconciles in three steps:

1. `GET /state` — fetch ephemeral control state (last `seq`, any pending gate,
   live processes, usage, effective policy). None of this is in the log.
2. `GET /log?since=<seq>` — replay durable history from a cursor.
3. `GET /events?since=<last_seq + 1>` — attach to the live durable tail; each
   event carries `id: <seq>`, so dedupe/resume by `seq`.

Optionally also `GET /stream` for live pre-commit output (lossy, no replay).

Ephemeral control state that has **no event** (the approval gate, process list,
usage, effective policy, pending proposals) is discovered only by **polling
`/state`**. The reference TUI polls it every **400 ms**.

---

## Endpoints

### Writes

#### `POST /turn`
Enqueue a user turn. Also seeds turn 1 on an idle daemon. Submission is
decoupled from inference.

- Body: `{ "text": "<string>" }`
- `202 {"status":"accepted"}` — enqueued; watch `/events` (or `/state`) for the result.
- `?wait` — hold the connection until the daemon **settles** (the turn reaches a
  gate or completes), then return the `/state` snapshot (`200`) instead of `202`.
- `400 {"error":"bad /turn body: …"}`

#### `POST /approvals/:call_id`
Resolve a pending approval gate, addressed by the tool call's `tool_use` id
(`call_id`).

- Body: `{ "decision": "approve" | "abort" | "redirect" | "config_edit", … }`
  - `approve` — run the call.
  - `abort` — reject; the turn unwinds.
  - `redirect` — reject; `"message"` is sent to the model as steering text.
  - `config_edit` — requires `"change"` (a `ConfigChange`), plus optional
    `"lifetime": "session"|"persist"` (default `session`) and `"note"`. The
    daemon folds the change into the effective policy and **re-classifies the
    gated call** against the new policy.
- `200 {"status":"resolved"}`
- `?wait` — hold until the turn's *next* gate or completion, then return `/state`.
- `404 {"error":"no pending approval with that id"}`
- `400` — bad body, unknown decision, or `config_edit` without a `change`.

#### `POST /interrupt`
Fire the interrupt notifier (the SIGINT equivalent; kills a running tool's
process group). No body.

- `200 {"status":"interrupted"}`

#### `POST /notify`
Enqueue an ambient notification into the inbox.

- Body: `{ "source": "<string>", "body": "<string>", "slot"?: "<string>", "replaces"?: <u64> }`
  - `slot` — a logical channel; a later notify on the same slot supersedes the
    pending one (archive-and-replace).
  - `replaces` — supersede a specific pending notification by its queue-local id.
- `202 {"id": <u64>}` — the new notification's **queue-local** id.
- `400 {"error":"bad /notify body: …"}`

#### `POST /notifications/:id/dismiss`
Remove a pending notification (by its queue-local id) before it reaches a gate.

- `200 {"status":"dismissed"}` · `404` (no such pending id) · `400` (id not an integer)

#### `POST /proposals/:seq`
Commit or cancel a pending config-edit proposal, addressed by the `seq` of its
`Proposed` log entry. The orchestrator-above decides; a session can only *file* a
proposal, never resolve its own.

- Body: `{ "decision": "commit" | "cancel", "note"?: "<string>" }`
  - `commit` folds the proposed change into the effective config (and notifies
    the session for `scope`/`paths` changes); `cancel` is audit-only.
- `200 {"status":"committed"}` / `{"status":"cancelled"}`
- `404` (no pending proposal with that `seq`) · `400` (bad body / bad decision) · `500` (append failed)

### Reads (polled)

#### `GET /state`
The ephemeral control snapshot. The single most-polled endpoint.

```jsonc
{
  "last_seq": 42,                  // u64 or null (empty log)
  "pending_approval": {            // null when no gate is open
    "call_id": "toolu_…",
    "name": "bash",
    "input": { … },                // the tool_use input, verbatim
    "reason": "…"                  // optional: the spotter's one-line reason
  },
  "autonomous": false,             // no synchronous human at the gate
  "role": {                        // EFFECTIVE policy = base config + folded edits
    "source": "/path/efference.toml",   // or null for the built-in default
    "scope": "…",
    "watch_for": [ … ],
    "commands": [ … ],
    "tools": { … },
    "paths": { "<path>": { "read": true, "write": false } }
  },
  "pending_notifications": [ { "id": 3, "source": "…", "body": "…", "slot": "…" } ],
  "proposals": [ { "seq": 17, "change": { … } } ],
  "processes": [ { …BgView… } ],   // live background processes
  "usage": {
    "last": { "input_tokens": …, "output_tokens": …,
              "cache_read_input_tokens": …, "cache_creation_input_tokens": … },
    "cumulative": { …same four fields… },
    "seconds_since_last_request": 12        // or null
  }
}
```

#### `GET /settle`
Long-poll: block until the daemon reaches a **decision point** (a gate is
pending, or the loop is idle with nothing queued), then return the `/state`
snapshot. Returns immediately if already settled. Capped server-side at **300 s**
(on timeout it returns current state; the client re-polls). This is the
push-shaped alternative to busy-polling `/state`, and the same predicate backs
the `?wait` flags on `/turn` and `/approvals`.

#### `GET /log?since=<seq>`
JSON array of `LogEntry` from `seq` onward (the whole log if `since` omitted).
The catch-up read.

#### `GET /proposals`
JSON array of pending proposals — the same array `/state` carries, for a client
that wants to poll only this.

### Reads (streamed, SSE)

#### `GET /events?since=<seq>`
Durable SSE tail of the log. Replays from `since` then follows live (whole
session if omitted). Per event:

- `event:` — the payload kind: `user` · `assistant` · `tool_result` · `mark` ·
  `triage` · `ambient` · `spot` · `config_edit`
- `id:` — the entry `seq` (the resume cursor: reconnect with `since = last_id + 1`)
- `data:` — the full serialized `LogEntry` (byte-identical to a `/log` element)

Slow-consumer safe: each connection tails the durable log at its own pace, so no
event is ever dropped.

#### `GET /stream`
Lossy SSE stream of **ephemeral pre-commit** output. **No `id`** and no replay —
a late subscriber starts from the next delta; a lagging one drops the oldest. The
committed entry on `/events` is the authoritative copy.

- `event: assistant_delta` — `{ "text": "<fragment>" }`
- `event: tool_output` — `{ "call_id": "…", "chunk": "…" }`
- `event: bg_output` — `{ "handle": "bg#<n>", "chunk": "…" }`

---

## Data shapes referenced above

- **`LogEntry`** `{ seq, turn, at, payload }` where `payload` is one of `User` /
  `Assistant` / `ToolResult` / `Mark` / `Triage` / `Ambient` / `Spot` /
  `ConfigEdit` (see [log-and-projection.md](log-and-projection.md)).
- **`ConfigChange`** — one of `command` / `path` / `tool` / `scope` (see
  [permissions.md](permissions.md)).
- **`BgView`** — a background-process view (see [supervisor.md](supervisor.md)).
- **`Usage`** — the four Anthropic token counters.

## Identifiers

| id | scope | issued by | used by |
|---|---|---|---|
| `seq` | durable, monotonic per session | the log | `/events` `id`, `/log?since`, proposal addressing |
| `call_id` | a tool call (the `tool_use` id) | the model | `/approvals/:call_id`, `tool_output` deltas |
| notification id | **queue-local**, not durable | the inbox | `/notify` response, `/notifications/:id/dismiss` |
| `bg#<n>` handle | a background process | the supervisor | `bg_output` deltas, process views |

## Error model

Non-2xx responses carry an ad-hoc `{"error":"<human string>"}` body. There is no
machine-readable error code, no problem-details envelope, and no content
negotiation. The CLI surfaces the status + body verbatim.
