# Web push — the daemon notifies a pocketed phone

Two moments mean a session is waiting on a person: an approval gate opened, and a turn
settled. Both happen inside a live daemon, so each daemon sends push for its own events
— there is no watcher process, and the host ([host.md](host.md)) is not involved.
Delivery is the Web Push triangle (RFC 8030): the browser's push service holds an
opaque endpoint per subscription, and the daemon POSTs an encrypted payload to it;
nothing ever connects inbound. Crash and stall notices are out of scope by design — a
process cannot report its own death, and that case belongs to a future outside
observer, not here.

## The sender identity is derived, not stored

The VAPID keypair (RFC 8292) comes off the machine root the same way every other
credential does ([web-bind.md](web-bind.md), `src/auth.rs`): HKDF under
`vapid v1 attempt-<n>`, where the counter increments until the 32 derived bytes are a
valid P-256 scalar (a uniform string exceeds the group order with probability ~2⁻³²;
the loop is deterministic per root). Every daemon under one root derives the identical
key with no coordination and nothing on disk. Rotating the root rotates the key, which
strands existing subscriptions until the service worker resubscribes on its next page
open — the same blast radius the cookie/bearer/TOTP family already has.

## The subscription store

`~/.efference/push.json` (`src/push.rs`), beside the TOTP burn ledger and
machine-scoped for the same reason: the key is the machine's, so every daemon can send
to every subscription, and a per-repo file would hide a subscription from the session
that ought to reach it. The file is a `{"subscriptions": [...]}` object; each entry is
what `PushSubscription.toJSON()` produced (endpoint, `p256dh`, `auth`). Writes are rare
— subscribe, unsubscribe, and pruning when a push service answers 404/410 — so the
whole file rewrites each time and the last writer wins; a lost concurrent write costs
one resubscribe. A missing or unparseable file reads as an empty list: a push that does
not go out must never fail a turn.

## The send

`src/push.rs` posts to the subscription endpoint with `TTL: 600` (both events mean
"now"; a gate answered an hour later is about a moment that has passed),
`Urgency: high`, `Content-Encoding: aes128gcm`, and a VAPID `Authorization` header. The
payload is `{"title", "body", "path"}`, encrypted per RFC 8291 (`src/push/encrypt.rs`)
— ECDH against the subscription's key, HKDF with the RFC's info strings, one
AES-128-GCM record. Every derivation step takes its salt and ephemeral key as
parameters, so the tests pin the RFC's §5 example and every Appendix A intermediate
byte-for-byte from the text in `reference/rfc/`. The JWT (`src/push/vapid.rs`) is
hand-assembled ES256 — two fixed JSON objects and a raw `r‖s` signature — with `aud`
the endpoint's origin and no `sub` claim: the only value `sub` could carry is a
personal contact handed to every push service.

Failures are log lines, never retried: a push is a courtesy, and no retry schedule
would make it more timely than the next event.

## When it fires, and what it says

`src/daemon/push.rs`. `Shared::push` short-circuits when no root is injected or no
runtime is running (every non-serving context — tests, offline verbs — looks like
that), then spawns the send so the turn loop never waits on a push service.

- **Gate** — the end of `register_pending` (`src/daemon/approvals.rs`). Body: the
  call's name and its `intent` — the model's own statement of purpose
  ([tools.md](tools.md)) — falling back to the spotter's reason (the risk, less useful
  at a glance than the purpose) and then to a plain waiting line, since no reader may
  assume `intent` is present.
- **Settle** — `parking()` (`src/daemon/pace.rs`), where `running.swap(false)`
  distinguishes a turn that just ended from boot idle; only the former fires. Body: the
  tail `Assistant` entry's prose, truncated to 200 characters. Only the tail entry is
  consulted — an errored tail yields an empty body rather than captioning this turn
  with an earlier one's text.

Titles are `<event> in <cwd>` — `Gate in ~/code/efference` — with the home directory
tilde-abbreviated; the event stands alone before `serve` sets an identity. The path in
the payload is `/` today, the session bind's own root; when the host grows its push
step ([../backlog/host-process.md](../backlog/host-process.md)) the sender carries the
session-prefixed path instead, and nothing else changes — the service worker opens
whatever path it is named.

## The routes

Under `/v1` with everything else ([control-api.md](control-api.md)), so the TCP gate
and the service worker's never-cache rule cover them without learning a new prefix:
`GET /v1/push` answers the VAPID public key (`applicationServerKey` verbatim);
`POST /v1/push/subscriptions` records a `toJSON()` subscription, upserting on the
endpoint; `DELETE /v1/push/subscriptions` forgets one, always 200 — an unsubscribe
names an end state.

## The receiving side

`web/public/sw.js` handles `push` (show the payload's title/body; a payload that does
not parse still shows a generic notification, since the platforms require a visible
notification per delivery) and `notificationclick` (focus a window already on the
payload's path, else open one). The handlers touch none of the caching rules above
them ([web-client.md](web-client.md)).

The opt-in is a "receive notifications" checkbox in the Interface tab
(`web/src/ui/Panel.tsx`). Its state derives from `pushManager.getSubscription()` — the
browser is the source of truth, not a stored preference — and the permission prompt
and subscribe run only from the click, which Safari requires and Chrome quiet-prompts
without. Unchecking unsubscribes both sides, daemon first. A denied permission renders
as "blocked in browser settings" (JS cannot re-prompt); iOS outside the
home-screen-installed app explains itself (iOS delivers push only to the installed
app); a browser without `PushManager` shows nothing.

## No push while someone is watching

A notification about a session the operator is already looking at is noise, so
`Shared::push` returns without sending when the session is **watched**: the count of
live `GET /events` connections is nonzero (`src/daemon/presence.rs`). The check sits
in `push` rather than at either call site, so a gate and a settle suppress
identically — they are one notification setting, enabled and disabled together — and
it is an atomic load, so it runs before the root and runtime checks above.

The count is connection lifetime, nothing else. It is incremented when a stream is
built and decremented by an RAII guard the stream holds, so it moves only with
connections and cannot drift: there is no presence report to send, no connection id
to name, and no per-connection visibility state to reconcile if a client dies
holding it.

The daemon learns a connection died only when a **write** to it fails — the next
event or heartbeat, up to one `/events` heartbeat interval after the client closed
(trillium-sse's write loop never polls the read half;
[../backlog/sse-close-detection.md](../backlog/sse-close-detection.md)). So an event
can be suppressed on the strength of a connection whose client already left, and the
event's own frame is what reaps it — one write too late, every time. The backstop:
a suppressed push is **stashed** rather than discarded, and the guard whose drop
takes the count to zero sends the stash after all if the suppression was recent
(`REPLAY_WINDOW`, sized to the client's hidden-tab linger plus one heartbeat).
Within that window, "the operator watched it happen" and "it fired into a dead
connection" are indistinguishable from the daemon, and a possible duplicate
notification beats a gate nobody was told about. Past the window, the suppressing
connection demonstrably outlived the daemon's blindness — a person had the event on
screen — and an event already seen is not re-notified.

What makes the count mean *watched* rather than merely *connected* is that clients
who are not being looked at drop their connections:

- **The web client** (`web/src/session/connect.ts`) drops its stream on
  `visibilitychange` to hidden, after a few seconds' linger that debounces a tab
  flip; becoming visible cancels a pending linger, or else reconnects through the
  same `?since=<cursor>` resume path a network loss already uses — hidden and
  offline repair identically, with no second reconnect. A tab restored in the
  background reads its initial visibility the same way, since no event fires for it.
- **The TUI** never drops its stream, so a connected TUI is always watched. Terminal
  focus is knowable, but occlusion is not, and an unfocused-but-visible terminal is
  a normal watching posture.
- **A non-human consumer** — an observer agent, a logging pipe — opens with
  `?presence=off` ([control-api.md](control-api.md)) and is excluded from the count.
  Static and open-time only: a stream does not become a human mid-connection.

**The service worker skips a notification a visible window would duplicate.** At
push-receive, `web/public/sw.js` reads `clients.matchAll()` and shows nothing when a
visible window is on the payload's path or under it (a prefix match, so any route
inside the session counts and a different session on the same host does not). That
covers the races the sender cannot see — the linger window, and a tab that has just
become visible and not yet reconnected. Chrome exempts this case from the penalty
for a push that shows nothing.

Watched is internal state, not a client-visible topic: any stream client that could
render it is itself a watcher, so it would read true by construction.
