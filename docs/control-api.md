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
2. `GET /log?since=<seq>` — replay durable history from a cursor. A client
   opening **cold** reads `?tail=<n>` instead: it renders immediately rather
   than after the whole session, and anything the window cut off is fetchable by
   address.
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
- `400 bad_request` — unparseable body

#### `POST /approvals/:call_id`
Resolve a pending approval gate, addressed by the tool call's `tool_use` id
(`call_id`).

- Body: `{ "decision": "approve" | "abort" | "redirect" | "config_edit", … }`
  - `approve` — run the call.
  - `abort` — reject; the turn unwinds.
  - `redirect` — reject; `"message"` is sent to the model as steering text.
  - `config_edit` — requires `"change"` (a `ConfigChange`), plus an optional
    `"note"`. The daemon writes the change to the session's config file, folds
    it into the effective policy, and **re-classifies the gated call** against
    the new policy. A failed write fails the grant.
- `200 {"status":"resolved"}`
- `?wait` — hold until the turn's *next* gate or completion, then return `/state`.
- `404 not_found` — no pending approval with that id
- `400 bad_request` — bad body, or `config_edit` without a `change`
- `400 invalid_decision` — a decision verb this daemon doesn't implement

#### `POST /interrupt`
Fire the interrupt notifier (the SIGINT equivalent; kills a running tool's
process group). No body.

- `200 {"status":"interrupted"}`

#### `POST /background`
Detach the running foreground tool into the background supervisor — the
foreground→background handoff ([supervisor.md](supervisor.md)). The command
keeps running as a `bg#<n>` process; the model's `tool_use` is paired with a
non-error handoff result. A no-op when nothing is mid-tool (the wake reaches
only a currently-running tool's race; a stale note is dropped when the next
foreground bash starts). Interim verb-route; the RESTful form is
`backlog/processes-rest-resource.md`.

- Body (optional): `{ "note"?: "<string>" }` — delivered to the model as a user
  message immediately after the handoff result. Blank/absent → silent handoff.
- `202 {"status":"detach requested"}`
- `400 bad_request` — unparseable body

#### `POST /notify`
Enqueue an ambient notification into the inbox.

- Body: `{ "source": "<string>", "body": "<string>", "slot"?: "<string>", "replaces"?: <u64> }`
  - `slot` — a logical channel; a later notify on the same slot supersedes the
    pending one (archive-and-replace).
  - `replaces` — supersede a specific pending notification by its queue-local id.
- `202 {"id": <u64>}` — the new notification's **queue-local** id.
- `400 bad_request` — unparseable body

#### `POST /notifications/:id/dismiss`
Remove a pending notification (by its queue-local id) before it reaches a gate.

- `200 {"status":"dismissed"}` · `404 not_found` (no such pending id) · `400 bad_request` (id not an integer)

#### `POST /proposals/:seq`
Commit or cancel a pending config-edit proposal, addressed by the `seq` of its
`Proposed` log entry. The orchestrator-above decides; a session can only *file* a
proposal, never resolve its own.

- Body: `{ "decision": "commit" | "cancel", "note"?: "<string>" }`
  - `commit` folds the proposed change into the effective config (and notifies
    the session for `scope`/`paths` changes); `cancel` is audit-only.
- `200 {"status":"committed"}` / `{"status":"cancelled"}`
- `404 not_found` (no pending proposal with that `seq`) · `400 bad_request` (bad body) ·
  `400 invalid_decision` (neither commit nor cancel) · `500 internal` (append failed)

### Reads (polled)

#### `GET /state`
The ephemeral control snapshot. The single most-polled endpoint.

Its keys are `last_seq` plus **exactly one field per state topic**, each named by the
topic. That is structural, not incidental: the same per-topic functions build this
snapshot and the level-triggered `state` frames, so the two cannot drift and `GET /state`
equals the fold of every `state` frame since connect. `last_seq` is the one field that is
not a topic — it is the log's cursor rather than control state, and sits outside the fold.

```jsonc
{
  "last_seq": 42,                  // u64 or null (empty log); NOT a topic
  "gate": {                        // null when no gate is open
    "call_id": "toolu_…",
    "name": "bash",
    "input": { … },                // the tool_use input, verbatim
    "reason": "…"                  // optional: the spotter's one-line reason
  },
  "role": {                        // EFFECTIVE policy = base config + folded edits
    "source": "/path/efference.toml",   // or null for the built-in default
    "scope": "…",
    "watch_for": [ … ],
    "commands": [ … ],
    "tools": { … },
    "paths": { "<path>": { "read": true, "write": false } }
  },
  "notifications": [ { "id": 3, "source": "…", "body": "…", "slot": "…" } ],
  "proposals": [ { "seq": 17, "change": { … } } ],
  "processes": [                   // live background processes: STRUCTURE ONLY
    { "handle": "bg#1", "command": "cargo watch -x test",
      "spawned_at": 1754300000000 }  // no tail, no last-chunk: both would re-emit per chunk
  ],
  "usage": {
    "last": { "input_tokens": …, "output_tokens": …,
              "cache_read_input_tokens": …, "cache_creation_input_tokens": … },
    "cumulative": { …same four fields… },
    "last_request_at": 1754300000000,       // absolute Unix millis, or null before the first request
    "context": {                            // null when no capabilities were fetched
      "limit": 200000,                      // the boot-fetched context window (transport.md)
      "used": 9578                          // last request's total prompt tokens; null before the first
    },
    "cost": {                               // null when the provider has no per-request accounting (transport.md)
      "usd": 0.0421,                        // billed session total from the provider's accounting endpoint
      "unpriced": 0                         // requests the fetch couldn't price (total is an undercount if > 0)
    }
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

#### `GET /log`
JSON array of `LogEntry`. The catch-up read. Unbounded by default — the whole
session — with three bounded forms:

| query | window |
|---|---|
| `?since=<seq>` | everything from `seq` onward (inclusive). The forward resume |
| `?tail=<n>` | the last `n` entries. How a client opens **cold** without replaying the session |
| `?before=<seq>&tail=<n>` | the `n` entries ending just before `seq`. One page backward |

`since`/`before` are positions; `tail` is a count. That separation is why the
tail read is its own parameter rather than a negative `since`: a parser cannot
tell a deliberate `since=-300` from a client's off-by-one `-1`.

- **`since` and `tail` together are a `400 bad_request`** — one asks to resume
  from a position, the other to open at the end, so the request has no single
  reading and the daemon does not pick one for you.
- A malformed bound (`?tail=abc`) is likewise a `400`, not ignored: dropping it
  would silently answer with the whole log.
- `?tail=<n>` means **exactly** `n`, **cut anywhere** — no boundary-snapping and
  no keyframes. The whole range when it holds fewer than `n`.
- `?before=<seq>` at or below the start of the log is an **empty array**, not an
  error: it is what a client paging backward naturally sends on its last page.

A window may be cut anywhere because references that dangle across the cut are
**fetched, not co-delivered** — that is what the next two routes are for. A
`tool_result` whose `ToolCall` sits in the entry before the cut, a
`MarkKind::Boundary` naming a span that precedes it, a `Triage` pinning an
`Ambient` from far back: all resolve by address.

#### `GET /log/:seq`
One `LogEntry`, or `404 not_found` past the head of the log. The general form of
reference resolution: every reference is ultimately to an entry or to something
inside one.

**`Cache-Control: immutable`.** A `LogEntry` is written once and never mutated —
redaction chooses `wire` at append time from the verdict rather than flipping it
afterward — so a returning client never refetches an entry it has seen. This is
a property a log-*window* read cannot offer, and it is the larger reason these
routes exist; reference resolution is the smaller one.

#### `GET /tool_calls/:call_id`
The tool call with that id, and the `seq` of the entry carrying it:

```jsonc
{ "seq": 41, "call": { "id": "toolu_…", "name": "bash", "input": { … } } }
```

A call is **not** its own log entry — it is embedded in an `Assistant` payload —
so it needs its own address, and `call_id` is the one already in the log, in
`tool_output` frames, in `/state`'s `gate`, and in the hand of any client holding
an orphaned `tool_result`. `404 not_found` if no entry carries it. Immutably
cacheable, for the same reason as `/log/:seq`.

The scale of the orphan problem is small by construction:
`disable_parallel_tool_use` means one call per turn
([transport.md](transport.md)), so a call sits at seq N and its result at N+1,
and a cut orphans **at most one result, at the boundary itself**.

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
- `event: thinking_delta` — `{ "text": "<fragment>" }` — extended thinking, never the
  assistant's answer; a client must render it as thinking or not at all.
- `event: tool_call_start` — `{ "name": "<tool>" }` — the model began emitting a tool call,
  named *before* its arguments stream (a large input takes seconds to arrive).
- `event: tool_call_input` — `{ "fragment": "<partial json>" }` — arguments arriving, verbatim.
  A client may display these but must not **parse** them as a complete document: mid-stream they
  are a partial slice.
- `event: tool_output` — `{ "call_id": "…", "chunk": "…" }`
- `event: bg_output` — `{ "handle": "bg#<n>", "chunk": "…" }`

The three middle events exist so a client can show *which phase* a turn is in. Without them
only prose reaches the stream, and a turn spent thinking or emitting a large tool input
publishes nothing at all — indistinguishable from a hung harness.

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

Every non-2xx response carries one shape:

```jsonc
{
  "code": "bad_request",              // machine-readable; branch on this
  "message": "bad /turn body: …",     // human half; free to change
  "detail": { … }                     // optional: corrective state, omitted when there is none
}
```

`code` is a closed set, and each code has one canonical status, so two routes
returning the same code cannot disagree about the status:

| code | status | when |
|---|---|---|
| `bad_request` | 400 | malformed body, unparseable path parameter, out-of-range value |
| `invalid_decision` | 400 | a well-formed decision verb the daemon doesn't implement — the client is version-skewed, not buggy |
| `not_found` | 404 | no such notification id, proposal `seq`, or pending approval |
| `internal` | 500 | a log append or config write failed; absent shared state |

Two further codes are defined and not yet returned: `gate_moved` (409) arrives
with `PUT /approvals/:call_id`, where `call_id` becomes a stated precondition and
`detail` carries the current pending gate; `cursor_out_of_range` (400) belongs to
the unified stream. A code earns its place when a client would *act* differently
on it, not when the prose differs.

The CLI surfaces the status + body verbatim.
