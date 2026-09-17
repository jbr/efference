# Ambient notifications — the inbox

The inbox is the realized form of "enqueue then triage": background and out-of-band
events interrupt the agent by enqueuing, but never mid-thought. Each time the agent
finishes an action and runs the triage fork peek (see `redaction-fork.md`), it grooms
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
control socket; the receiving agent sees the notification at its next triage gate exactly
as it would any other ambient event.

`GET /state` surfaces the pending (not-yet-shown) inbox alongside other control state.
`DELETE /notifications/:id` clears a pending notification out-of-band, before it
is ever shown — it is archived in the ephemeral queue and never reaches the log.

## Attachments — an arrival on disk is a notification

The attachment store is a **plain directory**, `.efference/<id>/attachments/`, beside
`config.toml` (`src/attachments.rs`, `src/daemon/attachments.rs`). A file appearing there —
pasted, dragged in, `mv`d, downloaded — is an attachment, whatever put it there. The daemon
runs a debounced `notify` watcher over the directory (the same machinery `config.toml` uses,
see [permissions.md](permissions.md)); each newly-arrived file is inspected and enqueued as an
ordinary `Notification` on the rail above. Files already present at boot are recorded as seen
and not re-announced.

**Nothing enters the log at arrival.** The bytes stay on disk; only the small notice rides
along, so a 50 KB paste never pins 50 KB into every subsequent turn's cached prefix
([log-and-projection.md](log-and-projection.md)). The model pulls what it wants with the tools
it already has — `read`/`search` for text, `read_image` for an image — and the bytes then enter
as an ordinary tool result, `Elidable` like any other. Because the store is only an ingestion
surface, nothing deterministic depends on it: a realized image is base64'd into its log entry at
pull time, so rearranging this directory can neither move committed wire bytes nor cold the
cache.

The notice states **what arrived and where**, and nothing about what to do with it. The harness
takes no stance on disposition: the user attached the file and the user can say what that
means, so there is no eager-read default baked in and no ask-first one either. Per-type triage
signals, chosen to be enough to decide *whether* to pull without re-deriving the content:

```
attachment at .efference/work/attachments/paste-1756345.txt
text · 51.2 KB · 1204 lines · looks like JSON
first line: {"model":"claude-opus-4","messages":[
```

- **text** — byte size, line count, a coarse sniffed shape (JSON / a diff / a stack trace /
  log output / prose), and the first non-blank line.
- **image** — format, pixel dimensions, byte size.
- **neither** — byte size alone.

The field set is prototype-first and tunable; the shapes are pinned by test, the heuristics
that produce them are not. The `slot` is the file's path, so a second arrival at the same name
supersedes the first notice rather than stacking a second one for the same bytes.

**Paste is a write to that directory.** The TUI composer inlines a paste of up to a dozen
lines and stores anything longer, and stores any clipboard image (re-encoded as PNG); the
watcher then describes it exactly as it describes a drag-in, so there is one path rather than
two ([tui.md](tui.md)). A colliding name is suffixed (`paste-2.txt`), never overwritten — an
attachment the model may not have read yet is not something a later arrival gets to replace.
The web client has no local filesystem to write to, so its paste routing waits on a daemon
upload endpoint (`backlog/attachments-remote.md`).

## Event-sourced, logged at first-show

A notification is appended to the log as a `Payload::Ambient` entry the moment a fork
peek first shows it to the model — not at arrival. A notification created and then
superseded or dismissed before any gate sees it never enters the log: from the log's
perspective it never happened.

Once shown, a notification is logged forever, dismissed ones included. The log is
therefore a full audit record: every notification the agent actually perceived, and
every disposition it chose, is reconstructable from the log alone.

## The inbox as a hybrid

The live inbox is the union of two layers:

- **Shown, unresolved**: a fold over the log, collected by `active_shown` in `src/inbox.rs`.
  Every `Ambient` entry whose `seq` no `Triage` verdict has yet pinned or dismissed is
  active. "Left" notifications — not named in any `Triage` inbox field — stay active and
  resurface at the next gate.
- **Pending, not yet shown**: the ephemeral queue in `Session`, layered on top.

A restart drops only the un-shown layer, which were "never happened" anyway. The
shown-and-active layer reconstructs from the log.

## Triage: pin · dismiss · leave

Pending and active-shown notifications appear in the fork peek view beneath the
line-numbered tool output, each item labelled with its arrival `seq` handle. The
`retain` verdict (see `redaction-fork.md`) gains an `inbox` field; the model may give
each notification one of three dispositions:

- **Pin** — promote the notification into canonical working context. It projects into the
  wire transcript at the pinning `Triage` entry's position (see below).
- **Dismiss** — clear it. The disposition is recorded; the notification no longer resurfaces.
- **Leave** — simply omit the item from the `inbox` field. The notification stays active
  and resurfaces at the next gate unchanged. This is the no-effort default; deferral costs
  nothing.

All dispositions are recorded on the `Triage` log entry (`inbox: Vec<(Seq, Disposition)>`),
making the fork's verdict durable even though the fork's transcript is throwaway.

## Must-wake — the disposition the harness declares

A notification may be enqueued **must-wake** (`wake` on the pending record, surfaced
on `GET /state` only when set). At its first show the loop pins it whatever the
fork's verdict — a dismiss is overridden, a leave is filled in — so an idle agent
escalates to a real turn on it and a running one carries it into the wire at the
next gate. Whether the agent wakes is thereby declared by the sender, not judged by
the peek; the peek keeps everything downstream of that: the rest of the inbox, the
background tails, its note. A peek that fails outright changes nothing here, since
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
position resolves to `upstream` ([permissions.md](permissions.md)) sends the call it is
holding to the session that created it, on the same `session:<id>` slot and with the same
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

This placement is what keeps pinning cache-safe. A notification may be shown at one gate,
left, left again, and pinned at a third gate. Projecting the block at the arrival position
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

The `⟢` sigil is chosen to be Haiku-safe. The `role:system` operator channel (the
eventual Opus-4.8 upgrade described in `supervisor.md`) is the anticipated path for
higher-trust injection; user-role is the conservative, universally-supported default.
