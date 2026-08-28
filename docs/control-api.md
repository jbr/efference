# Control API reference

> Consumer-facing reference for the daemon's control interface — written for
> someone building a *client* (the TUI, the CLI verbs, a future web/iOS/desktop
> app), not for someone changing the daemon. For the internal rationale and the
> `Frontend` boundary, see [control-channel.md](control-channel.md); for the
> streaming/transport internals see [transport.md](transport.md).
>
> **Status:** versioned, still pre-stability. Every route is served under the
> **`/v1`** prefix (`POST /v1/turns`); the paths in this document omit it. No
> compatibility promise yet — this documents what the daemon serves *today* so
> we can critique and harden it before more clients commit to it. A client
> should treat its control endpoint as an opaque base URL it joins paths onto:
> session identity arrives as an additive path prefix in front of these routes,
> never as a rename of them. `efference host` ([host.md](host.md)) is that
> prefix — `/sessions/<id>/` in front of everything below, forwarded to the
> session unchanged — so the same client code addresses a session daemon at a
> socket root or a host at a prefix, differing only in configuration.

## Transport & addressing

The daemon speaks **HTTP/1.1 over a Unix domain socket**. Each session owns a
directory `.efference/<id>/` (the id is `serve --name <id>`, or a generated
timestamp) holding `control.sock`, `log.jsonl`, and `config.toml`.

With `serve --bind`, the same routes are also served on a TCP listener behind a
hardening + auth gate — cookie or bearer, plus `401 unauthorized` /
`403 forbidden` / `429 rate_limited` in the standard error envelope, and the
`/auth/*` routes beside `/v1`. That listener also carries `/files` — the
operator's read-only view of the working directory, cookie-only and outside the
version prefix because it is a human surface rather than a control one. Both are
[web-bind.md](web-bind.md); the UDS stays auth-exempt and everything below is
unchanged on it.

The same routes are also reachable through `efference host` at
`/sessions/<id>/v1/…`, forwarded to that session's Unix socket byte-for-byte
behind the host's own credential ([host.md](host.md)). The host adds one endpoint
of its own — `GET /sessions`, the live-session listing — and none that translate
or enrich what is below.

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

Two requests, one of them long-lived:

1. `GET /log?tail=<n>` — a bounded window of durable history, so the client
   renders immediately rather than after replaying the session. Anything the
   window cut off is fetchable by address (`/log/:seq`, `/tool_calls/:call_id`).
2. `GET /events?since=<last_seq + 1>` — the control stream. Every subscribed
   state topic arrives on connect, then everything that changes: log entries,
   pre-commit output, and whole-topic state replacements, in one order.

Reconnect is the same two steps with a fresh cursor; `?since` is the only
handshake. There is nothing a client must poll — the stream's `state` class
carries the gate, processes, usage, and effective policy that used to require
it.

> **`GET /state` is still served** as a one-shot read (a script, a health
> check, the `efference state` verb). No shipped client polls it: the
> reference TUI holds one stream connection and derives everything from its
> frames.

---

## Endpoints

### Writes

#### `POST /turns`
Enqueue a user turn. Also seeds turn 1 on an idle daemon. Submission is
decoupled from inference.

- Body: `{ "text": "<string>", "submission_id": "<string, optional>" }` —
  `submission_id` identifies this submission: adopted when the client minted
  one (1–128 bytes), daemon-minted otherwise, and recorded on the resulting
  `User` entry as its `submission_id` (log metadata, never projected). A
  client that minted the id can settle its own delivery echo by exact key,
  and the log records which submission produced each turn.
- `202 {"status":"accepted","submission_id":"<string>"}` — enqueued; watch
  `/events` (or `/state`) for the result. The echoed id is informational: any
  effect of a request — including the `User` entry carrying this id — may
  arrive on `/events` **before** the response body resolves (the daemon
  commits and broadcasts independently of writing the response), so clients
  must not sequence state on a response.
- `?wait` — hold the connection until the daemon **settles** (the turn reaches a
  gate or completes), then return the `/state` snapshot (`200`) instead of `202`.
- `400 bad_request` — unparseable body, or a `submission_id` outside 1–128 bytes

#### `DELETE /turns/:submission_id`
**Recall** — withdraw the pending-queue **suffix** beginning at this submission,
returning what it actually popped. Nothing was ever logged (a pending turn sits
in the daemon's queue, unprojected), so there is nothing to retract; a draft is
not history.

A suffix rather than an arbitrary delete: recalling an older queued turn means
"pop up to and including that one", which is identical to popping repeatedly, so
queue order is preserved for free and there is exactly one operation.

- `200 {"withdrawn":[{"submission_id":"…","text":"…"}, …]}` — in submission
  order. The response is **authoritative**: the loop may have dequeued the turn
  between the read and this call, in which case fewer turns come back — possibly
  none, which is an ordinary race outcome and not an error. A client splicing the
  text back into a composer splices exactly this list, so a turn already on its
  way to the model is never handed back as a draft.

#### `PUT /approvals/:call_id`
Resolve the pending approval gate, addressed by the tool call's `tool_use` id
(`call_id`). The id is a **precondition**, not a lookup: the decision applies
only if that call is still the pending gate.

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
- `409 gate_moved` — that id is not the pending gate (already resolved, or the
  loop moved on). `detail` carries the **current** gate (`null` = none pending),
  so the client re-renders without a second round-trip. This closes the
  "approved a gate that already moved on" race.
- `400 bad_request` — bad body, or `config_edit` without a `change`
- `400 invalid_decision` — a decision verb this daemon doesn't implement

#### `GET /loop` · `PATCH /loop`
The turn engine's operator dials as one resource. Its fields are named for —
and are byte-identical to — their state topics: `pace` (`{speed_ms, held}`),
`review_level`, and the read-only `running`.

- `GET` → `200 { "pace": { "speed_ms": 15000, "held": false }, "review_level": "spotter_human", "running": true }`
- `PATCH` body: `{ "pace"?: { "speed_ms"?: <u64>, "held"?: <bool> }, "review_level"?: "<level>" }` —
  each field optional, so one call can move either dial or both (the two
  verb-routes this replaced could not). Validated whole before anything is
  applied: a part-bad request moves nothing.
- `PATCH` → `200` with the full resource representation.
- `400 bad_request` — bad body, or a `review_level` outside
  `autonomous | spotter_human | human_only`.

#### `POST /loop/continue`
Hand control back with **no input**: the idle loop takes another turn on the
wire it already has. Nothing is submitted, nothing is appended, and the
projection does not move — the next request re-sends a byte-identical prefix, so
this costs no input tokens beyond the turn it triggers. A verb-route on the loop
resource for the same reason as `/loop/interrupt`: it signals the loop rather
than creating or editing a resource. No body.

Distinct from a `POST /turns` carrying `""`, which is reserved for **keep
idling** — a blank submit never lands a `User` entry and never starts a turn.

- `202 {"status":"continuing"}` — the loop will take another turn. Like every
  other write, the effects arrive on `/events`; the daemon reads as busy from
  the moment it accepts (`GET /state?wait=settled` will not answer until the
  resulting turn settles).
- `409 not_continuable` — the moment has no continue in it. `message` names
  which case:
  - the loop is already running, or a turn is queued
  - an approval gate is pending — resolve it instead
  - the session has no turns yet (nothing projects)
  - the last turn completed (the wire ends in a final assistant response)
  - the last tool call has no result (the wire ends in an unanswered `tool_use`,
    a shape the API rejects)

The precondition is asked of the **wire**, not of the log: record-only entries
(an aborted attempt, a failed request, a `ConfigEdit`) project to nothing, so a
trajectory that gave up mid-stream still ends at the user turn or `tool_result`
it had before the failure and is continuable — which is the case the verb exists
for. A user-role tail means the model has not answered the last thing on the
wire; an assistant-role tail means it has.

Re-prompting a *completed* turn is left to the client to not ask for: whether an
identical wire produces anything but "I'm done" is an open question, and the
daemon declines rather than spending a round-trip on it.

#### `POST /loop/interrupt`
Signal the running tool (the SIGINT equivalent; kills its process group). A
verb-route on the loop resource because it *is* a verb — it signals, it doesn't
delete state. No body.

- `200 {"status":"interrupted"}`

#### `POST /notifications`
Enqueue an ambient notification into the inbox.

- Body: `{ "source": "<string>", "body": "<string>", "slot"?: "<string>", "replaces"?: <u64> }`
  - `slot` — a logical channel; a later notify on the same slot supersedes the
    pending one (archive-and-replace).
  - `replaces` — supersede a specific pending notification by its queue-local id.
- `202 {"id": <u64>}` — the new notification's **queue-local** id.
- `400 bad_request` — unparseable body

#### `DELETE /notifications/:id`
Remove a pending notification (by its queue-local id) before it reaches a gate.

- `200 {"status":"dismissed"}` · `404 not_found` (no such pending id) · `400 bad_request` (id not an integer)

#### `PATCH /proposals/:seq`
Move a pending config-edit proposal to its terminal state, addressed by the
`seq` of its `Proposed` log entry. The orchestrator-above decides; a session can
only *file* a proposal, never resolve its own.

- Body: `{ "state": "committed" | "cancelled", "note"?: "<string>" }`
  - `committed` folds the proposed change into the effective config (and
    notifies the session for `scope`/`paths` changes); `cancelled` is audit-only.
- `200 {"state":"committed"}` / `{"state":"cancelled"}`
- `404 not_found` (no pending proposal with that `seq`) · `400 bad_request` (bad body) ·
  `400 invalid_decision` (a `state` that is neither) · `500 internal` (append failed)

### The processes resource

Every running tool-spawned process — **foreground and background share one
namespace** ([supervisor.md](supervisor.md)): the foreground bash command is
registered and listed from the moment it spawns, and backgrounding is a
*property change* on it, not the appearance of a new resource. `:id` is the
bare spawn-minted ordinal (`GET /processes/3/output`); the `p#<n>` rendering is
prose-only and never appears in a path (`#` is the URL fragment delimiter — an
unencoded one silently truncates the request).

#### `GET /processes`
The running processes, as a JSON array of the same representation the
`processes` state topic carries — flagged, not filtered:

```jsonc
[ { "id": 1,                     // spawn-minted ordinal; rendered "p#1" in prose
    "command": "cargo watch -x test",
    "call_id": "toolu_01ab",     // the tool_use that spawned it, or null (supervisor.md); survives the handoff
    "pid": 4242,                 // OS pid — an attribute for direct interaction, never the identifier
    "spawned_at": 1754300000000, // absolute Unix millis; clients compute elapsed
    "background": true,          // false = the loop's in-flight foreground tool
    "muted": false } ]           // dismissed-from-view; still running, still notifies on exit
```

`call_id` is the call↔process mapping ([supervisor.md](supervisor.md)): a client
holding a transcript entry finds the process that answers it, and a client holding
a process row finds the entry to jump to. The listing carries only *running*
processes — a process leaving it is how an exit is represented, which is what
keeps it bounded over a long session — so a client asking about an exited one
addresses it directly, below.

#### `GET /processes/:id`
One process's record — running or exited. Once a handle is minted it stays
addressable at this one URL for the life of the session
([supervisor.md](supervisor.md)), including in a session whose daemon has
restarted or is long gone: the live registry answers when it has the process, and
a fold over the log's lifecycle records answers when it does not.

The body is the listing's representation plus the two fields it has no need for:

```jsonc
{ "id": 3, "command": "cargo watch -x test", "call_id": "toolu_01ab",
  "pid": 4242, "spawned_at": 1754300000000, "background": true, "muted": false,
  "running": false,             // false once it exited
  "exit": { "outcome": {"code": 1},   // or "signal" / "unknown"
            "duration_ms": 12000,
            "at": 1754300012000 } }   // null while it runs, and when no exit was recorded
```

- `spawned_at` is the process's spawn, not the entry's append — a command adopted
  by the handoff is recorded later than it started.
- A log-derived answer reads `running: false` and `muted: false`: the process is
  not in *this* daemon's registry, and mute is live-view state that was never
  durable. `exit: null` beside `running: false` says the end was never observed
  (the daemon died first).
- `404 not_found` — no process with that id was ever minted into the background
  this session. A log with no lifecycle records at all (one written before they
  existed) answers this way for every id.

#### `GET /processes/:id/output`
A cursor read of the process's retained output. With no cursor it returns
everything retained — **the seed a cold-opened client renders before its first
`process_output` delta** (the `output` class is lossy with no replay, so
without this read a client connecting mid-run shows an empty tail until the
next chunk). Works on exited processes: the capture outlives the exit.

- `?since=<cursor>` — resume after a previous read.
- `200 { "id": 3, "output": "…", "cursor": 5324, "lost": 0, "running": true }` —
  `cursor` is the value to pass next time; `lost` counts bytes between the
  caller's cursor and the earliest retained byte (dropped at the bounded cap —
  reported, never silently skipped).
- `404 not_found` — no process with that id was ever registered this session.

#### `PATCH /processes/:id`
Set a process's properties.

- Body: `{ "background"?: <bool>, "muted"?: <bool>, "note"?: "<string>" }`
- `{"muted": true}` / `{"muted": false}` — dismiss the running tail from view /
  undo that; applies synchronously, `200` with the representation.
- `{"background": true}` on the **foreground** entry is the
  foreground→background handoff ([supervisor.md](supervisor.md)): `202` with
  the representation, because the flip lands only when the detach wake reaches
  the running tool's race. On an already-background process it is an idempotent
  `200` no-op. An optional `note` rides the handoff and is delivered to the
  model as a user message right after the handoff result — a turn-injection
  concern beside the property change, not a process attribute.
- `400 bad_request` — `background: false` (no transition to name), or a `note`
  without `background: true`.
- `404 not_found` — no running process with that id.

#### `DELETE /processes/:id`
Kill a running process (SIGKILL to its whole process group). The drain task
then sees EOF and fires the exit notification like any other exit.

- `200 {"status":"killed"}` · `404 not_found` (no running process with that id)

### Reads (one-shot)

#### `GET /state`
The ephemeral control snapshot: the composite "current representation" the
stream pushes per-topic replacements of. A one-shot read for scripts and
bootstraps; a client holding the stream needs never request it.

**`?wait=settled`** turns the read into a long-poll: block until the daemon
reaches a **decision point** (a gate is pending, or the loop is idle with
nothing queued), then return the snapshot. Returns immediately if already
settled; capped server-side at **300 s** (on timeout it returns current state
and the client re-polls). The same predicate backs the `?wait` flags on
`/turns` and `/approvals`. A `wait` with a missing or unknown predicate is a
`400`, never answered immediately — a client that asked to wait must not be
told the present is what it waited for.

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
    "commands": { "<prefix>": "clear|watch|gate|deny" },
    "tools": { … },
    "paths": { "<path>": { "read": true, "write": false } }
  },
  "notifications": [ { "id": 3, "source": "…", "body": "…", "slot": "…" } ],
  "proposals": [ { "seq": 17, "change": { … } } ],
  "processes": [                   // every running tool-spawned process: STRUCTURE ONLY
    { "id": 1,                     // spawn-minted ordinal; rendered "p#1" in prose
      "command": "cargo watch -x test",
      "call_id": "toolu_01ab",     // the tool_use that spawned it, or null (supervisor.md)
      "pid": 4242,                 // OS pid — an attribute for direct interaction, never the identifier
      "spawned_at": 1754300000000, // no tail, no last-chunk: both would re-emit per chunk
      "background": true,          // false = the in-flight foreground tool (flagged, not filtered)
      "muted": false }
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
  },
  "queued": [                      // turns submitted but not yet dequeued, in submission order
    { "submission_id": "…",        //   `[]` when nothing is pending. Ephemeral — a pending
      "text": "…" }                //   turn is never logged, so this is the only place it is
  ],                               //   readable, and what `DELETE /turns/:id` addresses
  "stall": {                       // null unless the last trajectory gave up (transport.md)
    "reason": "retries_exhausted", // "retries_exhausted" | "request_failed"
    "attempts": 12,                // attempts spent, where the path counts them; else null
    "at": 1754300000000,           // absolute Unix millis at which the loop gave up
    "message": "the trajectory stopped: 12 attempts in a row aborted upstream — send a turn to retry",
    "detail": "upstream SSE stream failed mid-stream: connection reset"
  },
  "upstream_failure": {            // null once a 2xx arrives — the all-clear (transport.md)
    "status": 429,                 // the last non-success status; content-free by design
    "count": 2                     // consecutive responses carrying it
  },
  "upstream_retry": {              // null when nothing is being retried (transport.md)
    "surface": "turn",             // "turn" | "peek" — an exhausted peek withholds a
                                   //   verdict; an exhausted turn ends the trajectory
    "attempt": 2,                  // the attempt that just failed, 1-based
    "budget": 12,                  // attempts before the loop gives up
    "next_attempt_at": 1754300002000,  // absolute Unix millis; the client subtracts,
                                   //   so the daemon never re-emits a ticking field
    "message": "stream aborted, retrying (2 of 12)",  // leads with what is happening
    "detail": "upstream SSE stream failed mid-stream: connection reset"
  },
  "config_repair": {               // null whenever the config file parses (permissions.md)
    "path": "/repo/.efference/work/config.toml",  // the file a human has to fix
    "at": 1754300000000,           // absolute Unix millis at which the loop parked
    "message": "config unparseable — the loop is parked until a human repairs the file",
    "parse_error": "…",            // why the file did not parse
    "restore_error": "…"           // why writing the last good content back also failed
  },
  "session_identity": {           // null until `serve` sets it (tests, offline verbs)
    "model": "anthropic/claude-opus-4",  // the model id serve resolved, verbatim
    "cwd": "/Users/jbr/code/efference"   // absolute; shortening it is presentation
  }
}
```

**`stall` distinguishes the two idle conditions `running: false` collapses.**
When a turn's stream-abort budget is exhausted, or the request itself fails past
the transport's retries, the loop appends its record-only entries, posts the
turn-not-completed notification, and returns to idle
([transport.md](transport.md)). The daemon does not exit: the socket stays up
and one `POST /turns` recovers the session. But every other signal then reads
as a finished run — `running: false`, `gate: null`, and `notifications: []`
once a triage peek has dismissed the notice — so a supervising client cannot
tell "done" from "gave up". This topic is that difference. It carries the
reason, the attempts spent, the instant it stopped, a `message` in the same
lead-with-what-happened register as `upstream_retry`, and the upstream's own
words as `detail`. It is **cleared when a trajectory next starts**, not when one
succeeds, so it always names the current condition rather than a past one.

**How a supervising process watches for it.** There is no separate exit signal:
a stalled daemon is a live daemon, by design. Two shapes, both already here:

- **Long-poll** — `GET /state?wait=settled` blocks until the daemon reaches a
  decision point (a gate is pending, or the loop is idle with nothing queued)
  and returns the snapshot. A supervisor loops on it and branches on `stall`:
  `null` with no gate means the run finished; non-`null` means it stopped
  short, and `POST /turns` is the recovery. The 300 s server cap makes an
  unchanged answer a re-poll rather than a hang.
- **Subscribe** — `GET /events?include=state:stall,state:running` delivers the
  topic on connect and on every change, for a supervisor already holding a
  stream.

**`config_repair` is the one terminal condition the daemon holds.** It is set
only when a config edit both fails to parse *and* cannot be reverted, which
leaves the file on disk invalid while effective policy in memory is still the
last good config ([permissions.md](permissions.md)). The loop parks rather than
running on under a perimeter the file no longer shows, so unlike `stall` — which
a `POST /turns` recovers — nothing a client sends clears this one: a human edits
the file, fs-watch adopts it, and the topic goes `null` as the loop wakes. A
supervisor distinguishes it from `stall` on exactly that: `stall` names a
recovery it can perform, `config_repair` names one it cannot.

**`session_identity` names what the session is** — the model it was configured
with (the same string the system prompt's identity line states,
[context.md](context.md)) and the directory it runs in. Both are fixed at boot
today, so the topic is delivered on the connect snapshot and then stays put; it
is a topic rather than a one-shot read so a client learns it the way it learns
everything else, and so a value that ever moves reaches clients without a new
mechanism. Nothing else on `/v1` carries either, so without it a client cannot
say which session is on screen.

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

Every read that serves an entry — the window, this route, and a `log` frame's
`data` — serializes the same **entry view** (`daemon/entry_view.rs`): the entry
verbatim, except for two elisions, both resolved by fetching an address.

**A result image's base64 `data` is omitted, on every route.** A result image
appears as `{media_type, width, height}` — enough to lay out before fetching —
and the bytes resolve through the route below.

**A tool result's `content` past 16 KiB is truncated — in the window and on the
stream, but not here.** This route serves the whole string; it is the
dereference address for the truncated copies, exactly as the images route is for
image bytes. The truncated form is *tagged*, so a client never has to infer it
from a length:

```jsonc
"raw": {
  "content": "…the first 16 KiB…",
  "is_error": false,
  "content_elided": { "kept_bytes": 16384, "total_bytes": 262144 }
}
```

`content_elided` is **absent** when the content is whole — its presence is the
signal, and its `kept_bytes` is the prefix actually carried (cut back to a UTF-8
boundary, so it is a valid string rather than a split code point). A client
holding one renders the prefix, says what it is, and fetches `/log/:seq` when it
wants the rest.

Both elisions keep a bulky payload from riding every window, replay, and SSE
frame, where it would queue every later frame behind it on an expensively-framed
transport. The threshold is where the frame's own cost stops dominating the
fetch that replaces it: in this repo's own corpus (2,131 results), the median
result is ~1 KB and p90 ~13 KB, so 16 KiB elides ~8% of results and ~35% of all
result bytes — the tail, not the body. Below it, no route diverges from any
other.

The log file and the wire projection keep everything inline
([log-and-projection.md](log-and-projection.md)); genuinely *streaming* payloads
— assistant and thinking deltas — stay on the stream, since they arrive
incrementally rather than whole.

#### `GET /log/:seq/images/:idx`
The decoded bytes of one logged result image — the dereference target for the
image references entry views carry. `:seq` addresses the `ToolResult` entry,
`:idx` the image's position in its `images` array. The response is the stored
bytes under the stored media type (`Content-Type: image/webp` etc.), so the URL
works directly as an `<img src>` in the web client (the session cookie rides
along ambiently) and as a byte source for a TUI emitting a graphics-protocol
escape. Immutably cacheable, for the same reason as `/log/:seq`. `404
not_found` when the seq names no entry, the entry is not a tool result, or the
index is past its images.

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

#### `GET /tools/:name`
One tool's full definition, for **any** name the turn's `tools[]` can carry —
description, `input_schema` (with per-property descriptions and `required`),
`annotations`, `title`, and `server` (the mounting server's name, `null` for
one of the harness's own tools: `bash`, `read_image`, the fs/semantic-edit MCP
servers, `consolidate`, the supervisor/inbox family, the two peek tools).
Lazily fetched, one at a time, rather than a table pushed at session start: an
MCP server may mount dozens of tools with long descriptions, and a consumer
usually wants exactly the one behind a call it's showing.

`:name` accepts either spelling — the dotted operator form (`ferritin.get`)
or the wire form (`ferritin__get`) — through the same `wire_tool_name`
normalization the config layer applies to a `[tools]` grant
([permissions.md](permissions.md)); an already-wire name (no `.`) passes
through unchanged.

`input_schema` is served exactly as the model was sent it, not a
pre-injection schema, so a consumer comparing a call's arguments against
`required` sees the schema the call was actually judged against. That
distinction cuts one way: a `Toolbox`-mounted tool's schema carries the
injected [`intent`] property ([tools.md](tools.md) "The `intent` property"),
because `with_intent` runs over every entry `Toolbox::definitions` advertises
— but the harness's own loop-owned family (`consolidate`, the supervisor/inbox
tools, the two peek tools) is pushed onto the wire array *after* that, with no
injection, so their served schemas carry no `intent` property either.

This covers everything a session's `tools[]` can name — not just the
`Toolbox`'s mounted providers. A catalog built from the toolbox alone once
missed this whole loop-owned family (`check_inbox`, `list_processes`,
`consolidate`, `retain`, `triage` — dispatched structurally by the loop,
outside `Toolbox`), the same coverage gap the spotter once had over the same
family (`agent::brief`'s doc comment, [permissions.md](permissions.md)): a
lookup that consults only the providers reads a name in `tools[]` as absent.
The two peek tools (`retain`, `triage`) answer here even though they never
reach a gate and are never the subject of an ordinary logged call — they ride
every turn's `tools[]` regardless, so a reader inspecting the fork peek's own
exchange can still ask what shape it was answering.

Unlike the spotter's brief ([permissions.md](permissions.md)), this route has
no holes: `Provider::brief` returns `None` for `Bash` and `ReadImage` for
reasons specific to the spotter, but both have a definition, and both answer
here.

The mounted set is fixed for the session — `tools[]` sits in the cached
prefix ([log-and-projection.md](log-and-projection.md)) — so there is nothing
to reconcile: the daemon snapshots the full catalog once, before the turn loop
starts, and this route serves exactly that snapshot for the life of the
session. `404 not_found` for any name not in it, including *every* name in a
session read with no daemon behind it at all (a recorded session opened
offline) — that is the one case with no catalog to snapshot, and a client
falls back to showing only what a call sent, not an error state.

**A mounted server's description, annotations, and title are third-party
prose** — written by whoever wrote the server, not by efference
(`src/supervisor.rs` draws the same line for the built-ins' own annotations).
A client renders them as plain text, never markdown, and attributes them to
`server`.

### Reads (streamed, SSE)

#### `GET /events?include=<spec>&since=<seq>`
The control stream: **one** connection carrying everything that changes, as a
discriminated envelope so a client writes one frame parser rather than one per
stream.

```jsonc
{ "class": "log",    "kind": "tool_result", "seq": 43, "data": { /* LogEntry */ } }
{ "class": "output", "kind": "tool_output",            "data": { "call_id": "…", "chunk": "…" } }
{ "class": "state",  "kind": "gate",                   "data": null }   // null = topic cleared
```

The three classes are **durability classes**, not topics — durability is the axis
a client branches on when deciding whether a gap is recoverable:

| class | durability | resume |
|---|---|---|
| `log` | durable, monotonic `seq` | replay by `?since`; never dropped |
| `output` | ephemeral, lossy | none — live tail |
| `state` | level-triggered | snapshot on subscribe, then whole-topic replacements |

SSE fields: `event:` is the frame's `kind`, and `id:` is the `seq` — **on `log`
frames only**, so `EventSource`'s Last-Event-ID resume tracks the durable subset
and is never corrupted by an ephemeral frame. Reconnect with
`?since=<last_id + 1>`: the log replays exactly, state re-snapshots, output
resumes live with an accepted gap.

**`?include=`** takes comma-separated tokens, each either a bare class (`log`,
`output`, `state` — meaning all of it) or `class:topic` (`state:gate`). Omitted
means everything. A watch app subscribes `?include=state:gate,state:usage` and
gets neither the token firehose nor the whole effective config.

- **An unknown token is a `400 bad_request`, never ignored.** Silently dropping
  `state:gaet` would yield a stream that never delivers a gate — indistinguishable
  from a daemon that never gated. The cost is that an old daemon rejects a newer
  client, which is the right direction: the client learns.

**`state` is level-triggered, and a frame is a whole-topic replacement — never a
patch.** Every subscribed topic is sent on connect, then a topic's entire value is
re-sent when it changes (`null` = cleared). Replacement is idempotent and
order-insensitive, which is what makes this identity hold:

> **`GET /state` ≡ the fold of every `state` frame since connect** (minus
> `last_seq`, which is the log's cursor rather than control state).

Both sides are built from the same per-topic functions, so they cannot drift.
That identity is what lets a client delete its `/state` poll rather than merely
supplement it.

**Ordering — the guarantee three connections cannot offer.** Frames arrive in one
total order per connection, and a `state` frame follows every `log` frame it
reflects. So a client never sees the gate before the `spot` entry whose text the
gate quotes, and never sees `running: false` before the entries that finished the
turn — that second one answers "is it done?" wrong, in the direction a human acts
on. Mechanically: each state value is stamped with the log position it reflects,
and is held until the connection has delivered through it. A client subscribed to
`state` without `log` is not gated, having no log frames to be coherent with.

**Drain order is part of the contract, not an implementation detail.** One socket
means the ephemeral class *could* stall the durable one, which separate sockets
could not. So: `log` frames go unconditionally, `state` once its watermark is
passed, `output` only when nothing durable is pending, from a drop-oldest ring.
That policy is what makes the durability table above true over one connection.

**A committed entry preempts the deltas describing it.** Log priority means an
`Assistant` entry's frame overtakes the block deltas of the response it commits;
those deltas are then *dropped*, not delivered behind it. A client that folded
them would reopen a block the entry closed — a nameless tool call holding a
trailing `}`, or a turn's last sentence rendered twice. So: after a `log` frame
for an `Assistant` entry, the four block kinds (`assistant_delta`,
`thinking_delta`, `tool_call_start`, `tool_call_input`) do not arrive again until
the next log frame. `tool_output` and `process_output` are unaffected — they are
keyed by a call id and a process id rather than by a block index, and a
backgrounded process emitting while a turn commits must still reach the client.

This is a per-connection property of the daemon's drain, so a delta that already
left the daemon can still be in flight elsewhere in a client's own transport. A
client that buffers block deltas should keep dropping late ones as well; the
daemon's drop makes that the rare case rather than the normal one.

A client opening **cold** should read `GET /log?tail=N` and then connect with
`?since=<last_seq + 1>`, rather than `?since=0`. Both are correct; the second
replays the whole session, and its state waits behind that replay.

**A `?since` past the head waits; it does not error.** That is the natural
reading of a cursor on an append-only log: the entries simply haven't been
written yet. A client wanting the log's actual position reads `last_seq` from
`GET /state`.

**Heartbeat.** Whenever 15 seconds pass without a frame, the stream sends an SSE
comment (a `:` line) — the one keepalive an `EventSource` consumer discards
without dispatching anything, and the one that stays outside the frame envelope
(a heartbeat *frame* would need a fourth `class` naming no durability). It keeps
an idle stream from being reaped by a TLS intermediary, and it is what makes the
wait above distinguishable from a dead connection. The handler also negotiates
`Accept`: a request whose `Accept` header excludes `text/event-stream` passes
through rather than receiving a stream.

**The `output` class's kinds** — ephemeral pre-commit output, each frame's payload
under `data`. A late subscriber starts from the next delta; a lagging one drops
the oldest. The committed entry on the `log` class is the authoritative copy.

- `assistant_delta` — `{ "index": <n>, "text": "<fragment>" }`
- `thinking_delta` — `{ "index": <n>, "text": "<fragment>" }` — extended thinking, never the
  assistant's answer; a client must render it as thinking or not at all.
- `tool_call_start` — `{ "index": <n>, "call_id": "…", "name": "<tool>" }` — the model began
  emitting a tool call, named *before* its arguments stream (a large input takes seconds to
  arrive). `call_id` is the id its eventual `tool_result` and every per-call annotation key
  on, so a client can bind the live block to the call it becomes before the call commits.
- `tool_call_input` — `{ "index": <n>, "fragment": "<partial json>" }` — arguments arriving,
  verbatim. A client may display these but must not **parse** them as a complete document:
  mid-stream they are a partial slice.
- `tool_output` — `{ "call_id": "…", "chunk": "…" }`
- `process_output` — `{ "id": <n>, "chunk": "…" }` — a background process's output,
  keyed by the bare process id (rendered `p#<n>` in prose, never in machine-facing
  fields).

**Every delta names the item it appends to.** A streamed fragment is partial delivery *of a
specific item*, so the four model-stream kinds carry the `index` of the content block they
belong to, exactly as `tool_output` and `process_output` have always carried a `call_id` and
a process id. A client that cannot tell which item a fragment belongs to can only hold one
buffer per kind, and a response containing two tool calls then overwrites the first when the
second opens — the arriving arguments a client had been showing simply disappear. There is
no bound on how many blocks a response may contain, so this is a property of the protocol,
not a workaround for a provider quirk.

`index` is scoped to **the response now in flight**: it restarts at 0 per response, and the
loop issues one request at a time, so it is unambiguous without further qualification. The
committed entry on the `log` class supersedes the whole live set for that response.

The three middle kinds exist so a client can show *which phase* a turn is in. Without them
only prose reaches the stream, and a turn spent thinking or emitting a large tool input
publishes nothing at all — indistinguishable from a hung harness.

> `GET /stream`, which carried this class on its own socket, is retired: a delta
> taken there had no defined order against the log frames on the other
> connection — the exact hazard the unified stream removed.

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
| notification id | **queue-local**, not durable | the inbox | `/notifications` response, `DELETE /notifications/:id` |
| process id | a tool-spawned process; survives property changes | the supervisor, minted at spawn | the `/processes/:id` routes, `process_output` deltas, the `processes` topic; rendered `p#<n>` in prose |

## Error model

Every non-2xx response carries one shape:

```jsonc
{
  "code": "bad_request",              // machine-readable; branch on this
  "message": "bad /turns body: …",    // human half; free to change
  "detail": { … }                     // optional: corrective state, omitted when there is none
}
```

`code` is a closed set, and each code has one canonical status, so two routes
returning the same code cannot disagree about the status:

| code | status | when |
|---|---|---|
| `bad_request` | 400 | malformed body, unparseable path parameter, out-of-range value |
| `invalid_decision` | 400 | a well-formed decision verb the daemon doesn't implement — the client is version-skewed, not buggy |
| `not_found` | 404 | no such notification id, proposal `seq`, process id, or mounted tool name |
| `gate_moved` | 409 | `PUT /approvals/:call_id` whose id is not the pending gate; `detail` carries the current gate |
| `not_continuable` | 409 | `POST /loop/continue` at a moment with no continue in it — running, gated, or a wire tail that means nothing to re-prompt |
| `too_large` | 413 | `/files` was asked for a file past the view limit ([web-bind.md](web-bind.md)) |
| `unsupported_content` | 415 | `/files` was asked for something it does not serve as text — today, a file that is not valid UTF-8 |
| `internal` | 500 | a log append or config write failed; absent shared state |

A code earns its place when a client would *act* differently on it, not when
the prose differs. (A `cursor_out_of_range` code was once reserved for `?since`
beyond the head; it left when that case was settled as a wait rather than an
error.)

The CLI surfaces the status + body verbatim.
