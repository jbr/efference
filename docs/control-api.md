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
> never as a rename of them. A multi-session host is that prefix —
> `/sessions/<id>/` in front of everything below — so the same client code
> addresses a session daemon at a socket root or a host at a prefix, differing
> only in configuration. The host itself is planned work (`backlog/host-process.md`).

## Transport & addressing

The daemon speaks **HTTP/1.1 over a Unix domain socket**. Each session owns a
directory `.efference/<id>/` (the id is `--name <id>`, or a generated
timestamp) holding `control.sock`, `log.jsonl`, and `config.toml`.

With `host --bind`, the same routes are also served on a TCP listener behind a
hardening + auth gate — cookie or bearer, plus `401 unauthorized` /
`403 forbidden` / `429 rate_limited` in the standard error envelope, and the
`/auth/*` routes beside `/v1`. That listener also carries `/files` — the
operator's read-only view of the working directory, cookie-only and outside the
version prefix because it is a human surface rather than a control one. Both are
[web-bind.md](web-bind.md); the UDS stays auth-exempt and everything below is
unchanged on it.

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

### A host: the same routes under `/sessions/<id>/`

`efference host` ([control-channel.md](control-channel.md)) serves several
sessions from one process. Its own routes are the listener's, not a session's:

- `GET /sessions` — the listing:
  `[{"id": …, "cwd": …, "title"?: …, "created_by"?: …, "status": …, "processes": n}, …]`,
  the hosted sessions sorted by id, each with its working directory (what a
  client scopes the listing to its own project by,
  [control-channel.md](control-channel.md)), its current title once one has been
  written ([session-title.md](session-title.md)) and, when a session created it,
  the calling session's id as `created_by`. `created_by` is host control state —
  the creation tree — never logged or projected; a human-created root omits it
  ([tools.md](tools.md)). `status` is one word — `running`, `idle`, `gated`,
  `asking`, `stalled` — and `processes` counts the background processes the session holds
  ([supervisor.md](supervisor.md)). Those two are what a list surface can show
  without one `GET /sessions/<id>/v1/state` per row, and what a client's
  destructive affordance reads before it asks: ending a session that is doing
  something, or that holds processes an end would reap, is confirmed
  ([tui.md](tui.md), [web-client.md](web-client.md)).

  **The same path with `Accept: text/event-stream` subscribes to the listing.**
  One resource, two readings: a request naming the SSE media type receives the
  current listing as an `event: sessions` frame and the whole listing again on
  every change — a session arriving, leaving, or moving between those statuses.
  Any other request gets the JSON above.

  The stream is level-triggered and has **no durable class and no cursor**. A
  frame carries no `id:`, because there is no history to resume into: a late
  subscriber is caught up by the next frame, a missed frame is repaired by the
  next, and a reconnect is a fresh subscription. That is also why this is not a
  merge of per-session streams — the per-session contract is one cursor per
  handshake, and nothing here holds one. A client that wants a session's own
  entries opens that session's `/v1/events` beside this
  (backlog/host-process.md).
- `GET /inference` — the host's loaded connection inventory:
  `{"connections":[{"name": …, "provider": …, "available": bool}]}`. Rows are
  sorted by connection name. The response contains configuration status only; it never
  exposes endpoint URLs, credential sources, credentials, or environment values. It is
  available on authenticated Unix and TCP host listeners.
- `GET /inference/catalog?cwd=<root>&config=<path>` — every configured connection's
  model listing (docs/model-catalog.md):
  `{"connections":[{"connection", "provider", "status", "age"?, "error"?, "models":[…]}]}`,
  sorted by connection name. `status` is `fresh`, `stale`, `missing`, `failed`, or
  `unavailable` (no usable credential, or disabled: no read is attempted and `error`
  says why). `age` is seconds since the stored copy was fetched. Never waits on a
  provider: a `missing` or `stale` connection starts a background refresh and is
  served as stored; a `failed` one keeps its old copy in `models`. `cwd` (+ optional
  `config`) names the project scope whose recent picks fold in as `recent`; without
  `cwd` no row has one.
- `GET /inference/catalog/:connection?max_age=<secs>&cwd=&config=` — one connection's
  entry in the same shape, refreshed and awaited when its stored copy is missing or
  older than `max_age` (default 24 hours; values under 60 are raised to 60). A failed
  read answers `200` with `status: "failed"` and the old copy; an unknown connection
  is `404`; a non-numeric `max_age` is `400`.
  A row is `{"connection", "model", "group"?, "name"?, "created"?, "context"?,
  "price"?: {"input","output"}, "reasoning"?, "images"?, "recent"?}`. `model` is the
  provider-native id a draft carries; `created` is unix seconds; `context` is
  tokens; `price` is USD per million tokens; `recent` is the rank (1 first) of the
  matching recent pick, counted across connections. A field the source did not
  declare is absent.
- `QUERY /sessions` — resolve a proposed session without minting a directory or
  starting runtime preparation. The body carries an absolute `cwd`, an optional
  `config`, and the same editable selections creation takes: `connection`, `model`,
  `spotter_connection`, `spotter_model`, tagged `triage`, `temperature`, `effort`.
  A role names its connection and its provider-native model separately, or gives
  the `connection/model` slug as `model` alone — naming the connection is what
  makes a native id that itself contains a slash unambiguous. The response is

  ```json
  {"effective": {"main": {"value": {"connection": …, "provider": …, "model": …},
                          "source": "user_default"},
                 "spotter": {…}, "triage": {…}, "effort": {…}, "temperature": {…}},
   "issues": [{"field": "model", "code": "invalid_selection", "message": …}]}
  ```

  **`effective` is what a session would actually get**, field by field: the value
  and the precedence branch that supplied it. `source` is closed —
  `request`, `history`, `user_default`, `main_provider`, `decision_connection`,
  `project_config`, `provider_default`, `shared_main` — and only the branches that
  apply to a field occur for it.
  `triage` resolves to `{"mode": "shared"}` or a flattened separate selection, so
  an inherited `[defaults].triage` previews as the separate client creation would
  build rather than as sharing main.

  **Fields resolve independently.** One unusable value never empties another: a
  field with an issue has `value: null` *and* `source: null`, while a field nothing
  named has `value: null` with its source — the two are different answers. An
  invalid explicit override never silently falls back to a default. A value the
  host can *disprove* (a connection it has not configured, a temperature outside
  0–100) is an issue; one it merely cannot check (a model name it has no catalog
  for) resolves unquestioned, so an empty `issues` means only that nothing is known
  to be wrong. `code` is closed — `required`, `invalid_type`, `out_of_range`,
  `invalid_selection`, `unavailable_connection`, `incompatible_dialect`,
  `invalid_config` — and a config that will not load reports that it did not,
  never its contents.

  Only malformed JSON or a non-object body is `400`; every field-level problem is
  an issue inside a `200`. This is a preview, not a reservation: the host retains
  nothing, and a client sends its own overrides — not these effective values — to
  creation, which resolves again through the same flow (`inference/options.rs`).
  Available on every authenticated listener, Unix and TCP alike: it mints nothing
  and reads no environment.
- `POST /sessions` — create a session. The body is the session request: `cwd`
  (absolute, always sent), the `QUERY /sessions` selections above, and optionally
  `env`, `name`, `config`, `temperature`, `effort`, `upstream`, `autonomous`,
  `reauth`, `keep_scratch`, and `detached` (meaningful for a session-driven create
  only: whether the new session outlives its creator, docs/tools.md).
  `name` omitted mints an `adjective-noun` id free on the host and under the
  session base ([session-title.md](session-title.md)). `201 {"id": …}`; `400` with the reason when the body, the name, the cwd, the
  environment, or preparation is unusable, in which case nothing was minted.
  `env` is the session's complete base environment and is optional: supplied, it
  is used outright and no hook runs; omitted, the host establishes one for `cwd`
  from the user-wide `env_hook`, or from the environment the host itself started
  with when no hook is configured ([env.md](env.md)). An empty map is an explicit
  empty environment, not an omission. Mounted on every host listener, socket and
  network alike — a remote caller omits `env` and lets the host establish it, and
  that is the whole of what it changes. Extending the surface to a network
  listener is **not a security boundary**: this API is trusted over a secure
  channel, and a caller that can approve a gate can already have a session run
  anything. Nothing about creation widens that. The environment is applied and
  dropped, never logged or echoed.
  A creation whose client disconnects still completes and still mints; only the
  hook is bounded. Host shutdown cancels an establishment in progress, which fails
  the creation without minting.
  A creation that mints also records its directory in `GET /projects` — whatever
  asked for it, a web launch, a session's own `sessions__create`, or a `hand_off`
  in its predecessor's directory, since they are one path here — and one that
  fails records nothing. The write is best-effort like the model pick beside it:
  a store that cannot be opened never fails a creation that did start.
  One preparation step is not yet answerable by a network caller: an MCP server
  that answers `401` sends the harness into an authorization dance that prints to
  the host's stderr and opens a browser on the host's desktop, which that caller
  cannot see. Such a creation blocks until the authorization finishes or the host
  shuts down; a caller that disconnects does not cancel it, and nothing else
  bounds the wait. `backlog/remote-mcp-authorization.md` records the fix.
- `GET /projects` — the remembered launch locations: every successful `POST
  /sessions` records the directory it started in, keyed by that directory plus the
  *source* configuration the boot consumed (never the per-session copy under
  `.efference/<id>/`, whose path is per session). A relaunch refreshes the entry's
  timestamp rather than adding one, so the list reads most-recently-used first.
  There is no registration and no nickname: the `label` is the directory's own
  name, falling back to the full path at a filesystem root.
  `200 {"projects":[{"label", "cwd", "config", "config_key", "last_used_at"}, …]}`,
  at most 50 entries, where `cwd` and `config` are exactly what `POST /sessions`
  takes back (`config` `null` asks creation to probe, which is what a launch that
  named none did) and `config_key` is the identity the entry is stored under.
  The index **filters against the filesystem and writes nothing**: an entry whose
  `cwd` is not a directory, or whose *named* configuration is not a file, is left
  out — creation would refuse either ([permissions.md](permissions.md)) — while an
  absent *probed* `efference.toml` is not a reason to hide a directory, because
  creation falls back to its defaults there. Filtering is not forgetting: a
  directory on an unmounted volume is omitted until the volume returns. Creation
  validates again regardless; the check cannot close the race with a mount, and
  remembering a location never narrows which directories `POST /sessions` accepts.
- `DELETE /projects?cwd=<root>&config_key=<key>` — forget one entry, by the
  identity the index reported. `200 {"forgotten":{…}}`, `404 not_found` for an
  identity that is not remembered (which a client reads as "already gone"),
  `400 bad_request` when either parameter is absent. It deletes the remembered
  entry alone: no file, no directory, no session that ran there. The identity is
  taken as stored rather than recomputed from `cwd` and `config`, because
  recomputing re-probes the filesystem and a directory that gained an
  `efference.toml` since its launch resolves to a different key.
- `DELETE /sessions/:id` — end the session. Answers `200` once its teardown has
  run (background children reaped, scratch removed); the id leaves the list once
  the loop has returned. `404` for an id that is not hosted.
- `/sessions/:id/v1/…` and, on the web listener, `/sessions/:id/files/…` — that
  session's own routes, unchanged. The host resolves the session into conn state
  and dispatches into the same route table a session bind serves at its root;
  `404 not_found` for an unhosted id.

A client treats its control endpoint as an opaque base, so the session-prefixed
form and the socket-root form are the same client code with a different base.
In the Rust client that base is `Client::at(socket, "/sessions/<id>")`: the
prefix joins onto `/v1/…` only, while `POST /sessions` and the `/auth/*` family
are addressed at the socket root under either base, as the web client's base
joining does. The creation body is one type, `daemon::CreateSession`, serialized
by the client and deserialized by the host, so a new session knob is one field;
it has no Debug, because it carries the environment.

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
carries the gate, processes, usage, and effective policy in the same stream.

> **`GET /state` is still served** as a one-shot read (a script, a health
> check, the `efference control state` verb). No shipped client polls it: the
> reference TUI holds one stream connection and derives everything from its
> frames.

---

## Endpoints

### Writes

#### `POST /turns`
Enqueue a user turn. Also seeds turn 1 on an idle daemon. Submission is
decoupled from inference. While an `ask` is pending (the `ask` topic below), a
non-blank turn is instead that ask's reply: it becomes the call's result, never a
`User` entry, and never appears in `queued` ([supervisor.md](supervisor.md)).

- Body: `{ "text": "<string>", "submission_id": "<string, optional>", "source": "<string, optional>", "attachments": ["<name>", …] }` —
  `submission_id` identifies this submission: adopted when the client minted
  one (1–128 bytes), daemon-minted otherwise, and recorded on the resulting
  `User` entry as its `submission_id` (log metadata, never projected). A
  client that minted the id can settle its own delivery echo by exact key,
  and the log records which submission produced each turn.
- `source` names who is submitting, when it is not the operator — client-asserted,
  like `notify`'s source, and not verified. The convention is `web`,
  `mcp[<clientInfo.name>]`, or `agent[<id>]`. Recorded on the resulting `User`
  entry and rendered as a `source:` prefix line on its wire block
  (docs/log-and-projection.md); absent means the operator, whose turns render no
  prefix. `efference` is reserved for the harness's `first-turn` kickoff, which the
  clients do not draw ([context.md](context.md)); a request claiming it is a 400.
- `attachments` names files this client uploaded (`POST /attachments`, below) to
  send with the turn. Each is described at submission and lands on the `User`
  entry as an `Attachment` block after the text ([inbox.md](inbox.md)); a turn
  with attachments and blank text is still a turn. A name the store does not
  hold is a `400` and nothing is queued.
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

- `200 {"withdrawn":[{"submission_id":"…","text":"…","attachments":[…]?}, …]}` — in
  submission order; `attachments` (present only when the turn carried any) are the
  arrivals as described at submission, still in the store for the client to stage
  again. The response is **authoritative**: the loop may have dequeued the turn
  between the read and this call, in which case fewer turns come back — possibly
  none, which is an ordinary race outcome and not an error. A client splicing the
  text back into a composer splices exactly this list, so a turn already on its
  way to the model is never handed back as a draft.

#### `POST /attachments?name=<name>` · `GET /attachments/:name` · `DELETE /attachments/:name`
A composer's uploads ([inbox.md](inbox.md)). A client stages a file by uploading
it and names it in the `POST /turns` that sends it; uploading at staging time
means the client shows what the daemon made of the file while it is still
removable.

- `POST` — the raw bytes as the body, `Content-Type: application/octet-stream`
  (the one non-JSON type the TCP bind's CSRF gate admits, [web-bind.md](web-bind.md)),
  up to 64 MB. `?name=` suggests the file name; it is reduced to one path
  component and made unique (`shot-2.png`) rather than overwriting.
  `201 {"name","path","kind"}` — the stored name to send, the path the model
  reads it at, and its triage signals (`{"Text":{bytes,lines,shape,first_line}}`,
  `{"Image":{format,width,height,bytes}}`, or `{"Binary":{bytes}}`).
- `GET` — the bytes under their sniffed media type, `no-store`: unlike a log
  entry's image, a stored file can be removed. The thumbnail source for a sent
  turn's image.
- `DELETE` — `204`; for a staged file taken back before sending. A sent file
  stays in the store: its turn names it.
- `404 not_found` — a name that is not one this store handed out, or a session
  with no store.

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
  `autonomous | upstream | spotter_human | human_only`.

`review_level` is the dial's own value — what a client set and reads back. Who
answers a gate *right now* is the separate `resolved_review_level` topic on
`GET /state`, because `upstream` with no live upstream reviews as `spotter_human`
(see [permissions](permissions.md)).

#### `PUT /title`
Set the session's title by hand ([session-title.md](session-title.md)): one
`Title` log entry with `author: "operator"`, and the `session_identity` topic
moves with it. The title peek reads the new text as the standing title at its
next ask. Body `{ "title": "<one line>" }`; whitespace-trimmed, refused empty or
over the peek's own length cap, so both authors are held to one shape.

- `200` with the `session_identity` topic as it now reads.
- `400 bad_request` — bad body, empty title, or one over the cap; also a session
  no daemon booted, which has no identity to title.

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

  It is also **"retry now"** ([transport.md](transport.md)). While the loop is
  running but waiting on the upstream — an `upstream_retry` pause, or a triage
  gate held under a `stall` — the continue is accepted and ends that wait, so the
  next attempt fires at once; after a stall that returned the loop to idle, it
  re-sends the failed request. It can be repeated as often as the operator
  likes.
- `409 not_continuable` — the moment has no continue in it. `message` names
  which case:
  - the loop is already running (and not waiting on the upstream), or a turn is queued
  - an approval gate is pending — resolve it instead
  - a question is pending — answer it with a turn
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
Stop what the loop is doing now: a running tool is signalled (the SIGINT
equivalent; kills its process group) and the turn continues from its withheld
result; a generation in flight is ended — the stream is dropped, what streamed is
kept as a record-only entry, and the loop returns to idle, where a queued turn
lands at once. A no-op only when the loop is idle. A verb-route on the loop
resource because it *is* a verb — it signals, it doesn't delete state. No body.

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

#### `GET /push` · `POST /push/subscriptions` · `DELETE /push/subscriptions`
Web push ([web-push.md](web-push.md)). The GET answers the machine's VAPID public key
(`{"key": "<base64url uncompressed P-256 point>"}` — `applicationServerKey` verbatim);
the POST records a browser subscription in its `PushSubscription.toJSON()` shape,
upserting on the endpoint (`201`); the DELETE forgets one by `{"endpoint": ...}`,
always `200` — an unsubscribe names an end state. The store the writes land in is
machine-scoped, not per-session. All three answer `500 internal` when no root secret
is injected, a state unreachable through a bind that requires one.

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
reaches a **decision point** (a gate is pending, the loop is stalled — which
includes a triage gate held on the upstream, waiting on a continue — or the loop
is idle with nothing queued), then return the snapshot. Returns immediately if already
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
    "reason": "…",                 // optional: the spotter's one-line reason
    "acting_on": {                 // optional: the leaf a `sessions__approve` authorizes
      "name": "bash",              //   (docs/permissions.md) — the held call is the
      "input": { … },              //   parent's approval, this is the child's move
      "intent": "…",               //   optional
      "cwd": "/work",              //   the leaf's working directory
      "session": "child",          //   the session the parent's call directly names
      "chain": ["child", "grand"] //   that target down to the session holding the leaf
    }
  },
  "ask": {                         // null unless an `ask` is pending (docs/supervisor.md)
    "call_id": "toolu_…",
    "question": "…",
    "n": 1, "of": 2,               // its place among one completion's asks
    "recalled": [ … ]              // turns withdrawn from the queue when it began,
  },                               //   in the shape DELETE /turns/:id returns
  "role": {                        // EFFECTIVE policy = base config + folded edits
    "source": "/path/efference.toml",   // or null for the built-in default
    "scope": "…",
    "watch_for": [ … ],
    "commands": { "<prefix>": "clear|watch|gate|deny" },
    "tools": { … },
    "paths": { "<path>": { "read": true, "write": false } }
  },
  "notifications": [ { "id": 3, "source": "…", "body": "…", "slot": "…", "wake"?: true } ],
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
    },
    "plan": {                               // null off the Codex backend (transport.md)
      "type": "plus",                       // x-codex-plan-type
      "active": "codex_spark",              // exhausted/active limit id, or null
      "limits": {                            // independent meter families, keyed by normalized id
        "codex": {
          "name": null,                      // optional backend display name
          "primary": {                       // null when the last response omitted it
            "used_percent": 12,
            "window_minutes": 300,
            "resets_at": 1788677725000       // absolute Unix millis; the client subtracts
          },
          "secondary": { …same three fields… }
        },
        "codex_spark": { …same limit fields… }
      },
      "observed_at": 1788677700000          // when the last plan-bearing response arrived
    }
  },
  "queued": [                      // turns submitted but not yet dequeued, in submission order
    { "submission_id": "…",        //   `[]` when nothing is pending. Ephemeral — a pending
      "text": "…",                 //   turn is never logged, so this is the only place it is
      "attachments": [ … ] }       //   (only when it carries any)
  ],                               //   readable, and what `DELETE /turns/:id` addresses
  "stall": {                       // null unless the work stopped short (transport.md);
                                   //   `POST /loop/continue` retries it
    "reason": "retries_exhausted", // "retries_exhausted" | "request_failed" | "upstream_refused"
                                   //   | "triage_unavailable" | "triage_defect" — the triage
                                   //   reasons hold a gate with the loop still running
    "attempts": 12,                // attempts spent, where the path counts them; else null
    "at": 1754300000000,           // absolute Unix millis at which the loop gave up
    "message": "the trajectory stopped: 12 attempts in a row failed upstream",
    "detail": "upstream SSE stream failed mid-stream: connection reset"
  },
  "upstream_failure": {            // null once a 2xx arrives — the all-clear (transport.md)
    "status": 429,                 // the last non-success status; content-free by design
    "count": 2                     // consecutive responses carrying it
  },
  "upstream_retry": {              // null when nothing is being retried (transport.md)
    "surface": "turn",             // "turn" | "peek" — a peek waits between tool
                                   //   results; `POST /loop/continue` retries now
    "attempt": 2,                  // the attempt that just failed, 1-based
    "budget": 12,                  // attempts before the loop gives up
    "next_attempt_at": 1754300002000,  // absolute Unix millis; the client subtracts,
                                   //   so the daemon never re-emits a ticking field
    "message": "attempt 2 of 12 failed upstream, retrying",  // leads with what is happening
    "detail": "upstream SSE stream failed mid-stream: connection reset"
  },
  "config_repair": {               // null whenever the config file parses (permissions.md)
    "path": "/repo/.efference/work/config.toml",  // the file a human has to fix
    "at": 1754300000000,           // absolute Unix millis at which the loop parked
    "message": "config unparseable — the loop is parked until a human repairs the file",
    "parse_error": "…",            // why the file did not parse
    "restore_error": "…"           // why writing the last good content back also failed
  },
  "session_identity": {           // null until boot sets it (tests, offline verbs)
    "id": "amber-heron",                 // the session id — directory, --name, address (session-title.md)
    "title": "Session names: slugs and titles",  // the last Title entry's text; absent until one is written
    "model": "anthropic/claude-opus-4",  // the native model id, verbatim
    "cwd": "/Users/jbr/code/efference",  // absolute; shortening it is presentation
    "inference": {                       // hosted sessions only (docs/transport.md)
      "main":    {"connection": "work", "provider": "openrouter", "model": "anthropic/claude-opus-4"},
      "spotter": {"connection": "work", "provider": "openrouter", "model": "anthropic/claude-haiku-4-5"},
      "triage":  {"shared": true, "connection": "work", "provider": "openrouter", "model": "anthropic/claude-opus-4"}
    },
    "successor": "calm-finch"            // once this session has handed off (docs/tools.md); absent before
  }
}
```

**`stall` distinguishes the idle conditions `running: false` collapses.**
When a turn's stream-abort budget is exhausted, the request itself fails past
the transport's retries, or the upstream names a failure no retry can change
(`upstream_refused`, [transport.md](transport.md)), the loop appends its
record-only entries, posts the
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
  decision point (a gate is pending, the loop is stalled, or it is idle with
  nothing queued) and returns the snapshot. A supervisor loops on it and
  branches on `stall`: `null` with no gate means the run finished; non-`null`
  means it stopped short, and `POST /loop/continue` retries it (or `POST /turns`
  says something first). The 300 s server cap makes an
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

**`session_identity` names what the session is** — its id and title
([session-title.md](session-title.md)), the model it was configured with (the
same string the system prompt's identity line states, [context.md](context.md)),
and the directory it runs in. All but the title are fixed at boot; the topic is
delivered on the connect snapshot and republished whole whenever the title moves,
so a client learns it the way it learns everything else. Nothing else on `/v1` carries either, so without it a client cannot
say which session is on screen. Under a host, `inference` names the connection,
provider kind, and native model behind each role, and whether triage rides the
main client (`shared`, in which case the selection repeats main's, so two accounts
on one model stay distinguishable); a standalone session omits the field. The TUI
sidebar and the web top bar print the roles from it.

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

#### `GET /transcript`
The same window as `GET /log` — same `since`/`before`/`tail` query, same `400`s —
rendered as `text/plain` at reading grain (`daemon/transcript.rs`): one block per
entry, `seq tag body`, with a multi-line body indented to the body column. Prose
(`user`, `assistant`) is whole, except that a `user` entry carrying a `source`
opens its text with the same `source: ` address the wire uses
(docs/control-api.md); a `call` is its tool name, every argument but the
intent, then the intent in full — an argument named like a body (`contents`,
`content`, `text`, `body`, `old_string`, `new_string`) is cut short, any other at
a width that keeps a long `command` readable; a `result` is its outcome, size, and
wire verdict (`raw`, `elided`, `kept k/n lines: note`, or the triage failure), plus
how many images it carries — never its content. Calls that shared one assistant
entry are labelled `seq.1`, `seq.2`, … in place of the bare seq, and each of their
results opens with `for seq.n:`, so the pairing survives the spot and config
entries that interleave them; a call alone in its entry keeps the bare seq. An `ask`
call is not a `call` line: it is an `asked` line carrying the question, with
` (question n of m)` when its completion put more than one, and its result is a
`user` line with the reply's text (opening with the `source: ` address when the reply
was sourced) when the result entry carries a `submission_id`, or `result  not
answered` when it does not (an interrupt, a malformed call). `notify`, `spot`,
`config`, `process`, `mark`, `preamble` and `title` are one line each; a `triage`
line appears only when the verdict pinned or dismissed a notification or the peek
retried; telemetry renders to nothing. Every line's `seq` resolves through the
routes below, which is where the bodies are. The `transcript` control verb reads
this; the raw window is for clients that render entries themselves.

#### `GET /log/:seq`
One `LogEntry`, or `404 not_found` past the head of the log. The general form of
reference resolution: every reference is ultimately to an entry or to something
inside one.

**Validated, not immutable.** The router answers every successful GET with a
content-hash `ETag`, and a matching `If-None-Match` with `304 Not Modified`, so a
returning client's second look is one empty round-trip. No route sets a
`Cache-Control` lifetime. The `LogEntry` itself is written once and never
mutated — redaction chooses `wire` at append time from the verdict rather than
flipping it afterward — but what this route serves is the build's entry *view*,
which changes shape with the daemon, and at a session daemon's socket root the
address `/v1/log/3` is the same for every session that origin ever serves. A
year-long `immutable` lifetime once returned another session's entry to the web
client's inspector. The window read gets the same validator; its answer to a
query grows with the session, which the tag reflects.

Every read that serves an entry — the window, this route, and a `log` frame's
`data` — serializes the same **entry view** (`daemon/entry_view.rs`): the entry
verbatim, except for two elisions, both resolved by fetching an address, and one
addition a client cannot compute from an elided prefix.

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

**A distilled result's view carries the verdict's line arithmetic.** On every
route, a `ToolResult` whose `wire` is `Distilled` gains one field beside it:

```jsonc
"distillate": { "total_lines": 937, "kept_lines": 364 }
```

The counts are over the *whole* `content`, computed by the daemon through the
same `distillate` the wire and the TUI render from (`src/project/render.rs`), so
a client holding only the 16 KiB prefix states the same "kept 364 of 937 lines"
the model's view had — counted locally over the prefix, a verdict whose kept
ranges lie past it reads as "kept 0". Absent on a raw or elided result, so those
views stay the derived bytes. Derived on every read rather than stored: the log
is the record, and the view is where a count belongs. The per-line structure of
the distillate — which rows survived, numbered — is not served yet; the web
client still resolves that from the content it holds
([web-client.md](web-client.md) "The cordon").

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
escape. Validated by `ETag` like `/log/:seq`. `404
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
an orphaned `tool_result`. `404 not_found` if no entry carries it. Validated by
`ETag` like `/log/:seq`.

The scale of the orphan problem is bounded by the batch: a completion's calls
share one assistant entry, and their results are appended together after the
gate ([README.md](README.md)), so a cut orphans **at most one batch of results,
at the boundary itself**.

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

#### `GET /events?include=<spec>&since=<seq>&presence=<on|off>`
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

**`?presence=`** is `on` (the default) or `off`. A live connection is what tells
the daemon someone is watching the session, which suppresses push
([web-push.md](web-push.md)); `off` excludes this connection from that count, for a
consumer that is not a person — an observer agent, a logging pipe. It is read at
open and never changes for the life of the connection. Any other value is a
`400 bad_request`, on the same rule as an unknown `include` token: a client that
meant to opt out and misspelled it would be counted as a watcher and would suppress
the notifications it connected to observe.

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

- **`LogEntry`** `{ seq, turn, at, submission_id?, source?, payload }` where
  `payload` is one of `User` / `Assistant` / `ToolResult` / `Mark` / `Triage` /
  `Ambient` / `Spot` / `ConfigEdit` (see
  [log-and-projection.md](log-and-projection.md)). `submission_id` names the
  `POST /turns` that produced the entry; `source` names its submitter when not
  the operator and is the one entry field that reaches the wire.
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
