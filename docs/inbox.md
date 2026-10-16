# Ambient notifications — the inbox

The inbox is the realized form of "enqueue then triage": background and out-of-band
events interrupt the agent by enqueuing, but never mid-thought. Each time the agent
finishes an action and runs triage (see `triage.md`), it grooms
its inbox in the same throwaway branch at no extra round-trip.

## What a notification is

A `Notification` carries three fields: `source` (a label for the origin), `body`
(freeform text), and an optional `slot` (a logical channel name used for supersession).
There is no structure beyond that — the body is prose the model reads directly.

## Ingestion — out of band

`POST /notifications { source, body, slot?, replaces? }` enqueues the notification into an
**ephemeral** inbox queue held in `Session` (the daemon's shared control state). Nothing
is written to the log at arrival; the notification exists only in memory. This is the
same pattern as pending-approval state (see `control-channel.md`): in-flight inbox state
is control state until the log needs it.

This verb is also the inter-agent channel. One agent `POST /notifications`s another agent's
control socket; the receiving agent sees the notification at its next triage exactly
as it would any other ambient event.

A connected MCP server is another in-process producer: each of its
`notifications/message` lines is enqueued through the same path, with the
server name as `source` and `mcp:<server>` as `slot`, so a burst supersedes
itself instead of piling up ([tools.md](tools.md) "the client protocol"). Its
progress ticks do not come here — they are live output on the running call,
and by the time triage runs they describe the past.

`GET /state` surfaces the pending (not-yet-shown) inbox alongside other control state.
`DELETE /notifications/:id` clears a pending notification out-of-band, before it
is ever shown — it is archived in the ephemeral queue and never reaches the log.

## Attachments — the store, and two ways into it

The attachment store is a **plain directory**, `.efference/<id>/attachments/`, beside
`config.toml` (`src/attachments.rs`, `src/daemon/attachments.rs`). Files reach it two ways, and
both are described the same way — by a **notice** stating what arrived and where — but they
are announced on different rails:

- **A composer attaches it to a turn.** The TUI and the web client upload a staged file
  (`POST /attachments`, [control-api.md](control-api.md)) into `attachments/composer/`, and the
  turn that sends it names the upload (`POST /turns {attachments: [name…]}`). The daemon
  describes each named file at submission and the `User` entry carries one
  `ContentBlock::Attachment` per file after its text; projection renders each as its notice, in
  the turn's own user message. A name the store does not hold refuses the whole turn, so a
  message never lands claiming a file that is not there. A staged file removed before sending
  is deleted (`DELETE /attachments/:name`); a sent one stays, because its turn names it.
- **Something writes it into the directory's top level** — dragged in with a file manager,
  `mv`d, `curl -o`'d. The daemon runs a debounced `notify` watcher over the top level (the same
  machinery `config.toml` uses, see [permissions.md](permissions.md)); each newly-arrived file is
  inspected and enqueued as an ordinary `Notification` on the rail above. Files already present
  at boot are recorded as seen and not re-announced. The watcher reads regular files only, so
  it never sees `composer/` — which is what keeps an uploaded file from being announced twice.

**Nothing enters the log but the notice.** The bytes stay on disk, so a 50 KB paste never pins
50 KB into every subsequent turn's cached prefix ([log-and-projection.md](log-and-projection.md)).
The model pulls what it wants with the tools it already has — `read`/`search` for text,
`read_image` for an image — and the bytes then enter as an ordinary tool result, `Elidable` like
any other. An image attached to a turn is a notice too, not an image block: pulling it through
`read_image` is what lets the model choose its crop and detail. The store is readable through
the self-surface deny on `.efference/` (a read-only re-allow injected at boot beside
`config.toml`'s, [permissions.md](permissions.md)), so a pull clears at the fast path rather than
gating. Nothing deterministic depends on the directory: a realized image is base64'd into its log
entry at pull time, and an attachment block records the file's name, path, and signals, so
rearranging the directory can neither move committed wire bytes nor cold the cache.

The notice states **what arrived and where**, and nothing about what to do with it. The harness
takes no stance on disposition: the user attached the file and the user can say what that
means, so there is no eager-read default baked in and no ask-first one either. Per-type triage
signals, chosen to be enough to decide *whether* to pull without re-deriving the content:

```
⟢ attached: paste-1756345.txt — text, 1204 lines, 51.2 KB, looks like JSON — at .efference/work/attachments/composer/paste-1756345.txt
first line: {"model":"claude-opus-4","messages":[
```

One line, subject first, under the harness's `⟢` — the turn's own prose is the operator's, and
the attachment line is the harness's report of what came with it. The watcher's notices carry
the same body as `⟢ attachment: …`. The earlier two-line form, with the path leading, read as
boilerplate: in one dogfood session the model later claimed the dimensions had never reached it,
and took the notice on a later turn for the operator quoting something
(journal/2026-09-27-composer-attachments.md).

- **text** — byte size, line count, a coarse sniffed shape (JSON / a diff / a stack trace /
  log output / prose), and the first non-blank line.
- **image** — format, pixel dimensions, byte size.
- **neither** — byte size alone.

The field set is prototype-first and tunable; the shapes are pinned by test, the heuristics
that produce them are not. For the watcher's notices, the `slot` is the file's path, so a second
arrival at the same name supersedes the first notice rather than stacking a second one for the
same bytes.

**What a paste becomes** is the same rule in both clients: a pasted file or image is staged; so
is text past a dozen lines (`attachments::INLINE_LINE_LIMIT`), as a `.txt` the model can grep
rather than a wall in the draft; anything shorter pastes into the composer. The TUI also stages
a drag onto the terminal, which arrives as a paste of escaped absolute paths
([tui.md](tui.md)); the web client stages a drop anywhere on the page
([web-client.md](web-client.md)). A colliding name is suffixed (`paste-2.txt`), never
overwritten — an attachment the model may not have read yet is not something a later arrival
gets to replace.

## Event-sourced, logged at first-show

A notification is appended to the log as a `Payload::Ambient` entry the moment a
triage first shows it to the model — not at arrival. A notification created and then
superseded or dismissed before any triage sees it never enters the log: from the log's
perspective it never happened.

Once shown, a notification is logged forever, dismissed ones included. The log is
therefore a full audit record: every notification the agent actually perceived, and
every disposition it chose, is reconstructable from the log alone.

## The inbox as a hybrid

The live inbox is the union of two layers:

- **Shown, unresolved**: a fold over the log, collected by `active_shown` in `src/inbox.rs`.
  Every `Ambient` entry whose `seq` no `Triage` verdict has yet pinned or dismissed is
  active. "Left" notifications — not named in any `Triage` inbox field — stay active and
  resurface at the next triage.
- **Pending, not yet shown**: the ephemeral queue in `Session`, layered on top.

A restart drops only the un-shown layer, which were "never happened" anyway. The
shown-and-active layer reconstructs from the log.

## Triage: pin · dismiss · leave

Pending and active-shown notifications appear in the triage view after the ask, under the
riders header that marks them as not the tool output (docs/triage.md), each item labelled
with its arrival `seq` handle. The
`retain` verdict (see `triage.md`) gains an `inbox` field; the model may give
each notification one of three dispositions:

- **Pin** — promote the notification into canonical working context. It projects into the
  wire transcript at the pinning `Triage` entry's position (see below).
- **Dismiss** — clear it. The disposition is recorded; the notification no longer resurfaces.
- **Leave** — simply omit the item from the `inbox` field. The notification stays active
  and resurfaces at the next triage unchanged. This is the no-effort default; deferral costs
  nothing, up to a bound: a notification that no verdict mentions across
  `MAX_UNMENTIONED_SHOWS` (3) triages is dismissed by the harness (below).

All dispositions are recorded on the `Triage` log entry (`inbox: Vec<(Seq, Disposition)>`),
making the fork's verdict durable even though the fork's transcript is throwaway.

## Unmentioned notifications expire

Without a bound, a notification no verdict mentions is shown at every triage for the rest
of the session (one session re-showed a test-exit notice from seq 423 to seq 467). After the third
triage that shows a notification and does not pin or dismiss it, the loop appends
`Telemetry::InboxExpired { seq, shows }` and the notification leaves `active_shown`, as a
`Dismiss` would. The entry projects to nothing and records that the harness, not the
model, made the choice. `unmentioned_past_cap` in `src/inbox.rs` folds the count from the
log: one show per idle `Triage`, and one per run of result-carrying `Triage` entries with
no `Assistant` or `User` entry between them (a batch writes one per result, and the
verdict rides the last). The check runs after each triage's entries are written.

## Must-wake — the disposition the harness declares

A notification may be enqueued **must-wake** (`wake` on the pending record, surfaced
on `GET /state` only when set). At its first show the loop pins it whatever the
fork's verdict — a dismiss is overridden, a leave is filled in — so an idle agent
escalates to a real turn on it and a running one carries it into the wire at the
next triage. Whether the agent wakes is thereby declared by the sender, not judged by
the triage; the triage keeps everything downstream of that: the rest of the inbox, the
background tails, its note. A triage that fails outright changes nothing here, since
the pin is applied by the loop after the verdict returns. An escalated trajectory
marks the loop `running` itself, since no submitted turn was claimed to do it: from
the escalation until the loop parks, `/settle` blocks and the status watcher reports
running, the same as for a trajectory a turn started.

The flag is ephemeral, like the queue: the logged `Ambient` does not carry it, and the
`Triage` records the pin as a pin. No client verb sets it — `POST /notifications`
enqueues ordinary notifications — it is the harness's own channel, used today for a
delegated child's status ([tools.md](tools.md), subscriptions). Superseding on a slot
takes the replacement's own terms.

**A routed gate rides this channel, and is not a subscription.** A session whose review
position resolves to `supervisor` ([permissions.md](permissions.md)) sends the call it is
holding to its supervisor (by default the session that created it), on the same `session:<id>` slot and with the same
must-wake, whether or not that session is subscribed — and with a body that names the
recipient as the one to resolve it. Everything else a subscriber hears is observation: the
same statuses, on the subscription's own terms, which `unsubscribe` drops. Keeping the two
apart is what lets an agent stop watching a peer without also stopping answering for it, and
stop answering for a child without going blind to whether it finished.

## Supersession — archive-and-replace, never edit

Records are immutable; a `PUT /notifications/:id` verb does not exist. A new notification
naming the same `slot` (or naming an explicit `replaces` id) archives the prior record
rather than mutating it. For example, a slot `bash#7` receiving "running 5 min" and then
"finished" produces two immutable records, not one edited one. The wire prefix therefore
re-renders byte-identically across turns (the core caching invariant from `log-and-projection.md`).

Superseded while pending: the prior record is archived in the ephemeral queue and never
reaches the log — the never-happened case. Superseded after pinning: the new notification
tail-appends, so the wire reads as an honest timeline (running… → finished…), never a
rewrite.

## Projection position — why Triage, not arrival

Pinned notifications project into the wire transcript at the **pinning `Triage` entry's
position**, not at the arrival entry's position. Arrival entries (`Payload::Ambient`)
project to nothing — they are audit-only.

This placement is what keeps pinning cache-safe. A notification may be shown at one triage,
left, left again, and pinned at a third triage. Projecting the block at the arrival position
would splice new content behind an earlier point in the transcript — behind the cache head
if any boundary has been committed — invalidating the cached prefix (see `log-and-projection.md`).
Projecting at the `Triage` position is always tail-ward: the wire grows only at the tail.

It is also semantically correct: the agent "receives" a notification when it acknowledges
it, not when it arrived.

## Wire rendering

A pinned notification renders as a user-role message block:

```
⟢ <source>: <body>
```

The `⟢` sigil is chosen to be Haiku-safe. The preamble says what it means — notes the harness
adds, which report and never carry the operator's instructions (`assets/prompts/session.md`) —
and every harness note shares it: pinned notifications, attachments, surfaced context, the
triager's note on a result. It is a convention, not an authentication: anything that can put
text in front of the model (a file, a command's output, another client's turn) can spell it.
What keeps a forged note harmless is that a genuine one carries no authority to borrow — gates
are enforced mechanically, never by what the model believes — so a forgery can plant a false
fact but not an instruction. The one `⟢` block that asks for something, the triage ask
(docs/triage.md), asks only for `retain` or `triage`; outside a triage either call is
answered with a "nothing is being asked" stand-in, so a forged ask costs a round-trip. The `role:system` operator channel (the eventual Opus-4.8 upgrade
described in `process-monitor.md`) is the anticipated path for anything that *should* carry
authority; user-role is the conservative, universally-supported default
(backlog/inbox/harness-note-authority.md).
