# Web push — the daemon notifies a pocketed phone

Three moments mean a session is waiting on a person: an approval gate opened, an `ask`
was posed, and a turn settled. All happen inside a live daemon, so each session sends
push for its own events — there is no watcher process.
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
key with no coordination. Rotating the root rotates the key, which
strands existing subscriptions until the page resubscribes on its next
open — the same blast radius the cookie/bearer/TOTP family already has.

## The subscription store

The `push_subscriptions` table of the user state database ([state-store.md](state-store.md),
`src/push.rs`), machine-scoped for the same reason the burn ledger is user-scoped: the
key is the machine's, so every daemon can send to every subscription, and a per-repo
store would hide a subscription from the session that ought to reach it. One row per
subscription, keyed by its endpoint — what `PushSubscription.toJSON()` produced
(endpoint, `p256dh`, `auth`). Writes are rare: subscribe, unsubscribe, and pruning when
a push service answers 404/410, each touching one row. A table that cannot be read is an
empty list, and a push that does not go out must never fail a turn.

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

`src/daemon/push.rs`. `Session::push` decides whether to send now or hold the
message ([below](#no-push-while-someone-is-attending)); sending short-circuits when
no root is installed (only a binding host installs one — tests, offline verbs, and a
socket-only host look alike) or no runtime is running, and spawns the send so the turn
loop never waits on a push service.

- **Gate** — the end of `register_pending` (`src/daemon/approvals.rs`). Body: the
  call's name and its `intent` — the model's own statement of purpose
  ([tools.md](tools.md)) — falling back to the spotter's reason (the risk, less useful
  at a glance than the purpose) and then to a plain waiting line, since no reader may
  assume `intent` is present.
- **Ask** — `begin_ask` (`src/daemon/ask.rs`). Body: the question, truncated.
- **Settle** — `parking()` (`src/daemon/pace.rs`), where `running.swap(false)`
  distinguishes a turn that just ended from boot idle; only the former fires. Body: the
  tail `Assistant` entry's prose, truncated to 200 characters. Only the tail entry is
  consulted — an errored tail yields an empty body rather than captioning this turn
  with an earlier one's text.

Titles are `<event> in <cwd>` — `Gate in ~/code/efference` — with the home directory
tilde-abbreviated; the event stands alone before boot sets an identity. The path in
the payload is the session's web path ([web-bind.md](web-bind.md)) — `/` on the
session's own bind, `/sessions/<id>/` under a host — the same path the client's own
links name, so a notification opens the session it announces. The service worker
opens whatever path it is named.

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

## Self-healing subscriptions

A browser holds its subscription independently of the daemon's store, so the two
can disagree after a store loss, a pruned row, or a rotated root. On every app
open, once the worker is ready, the page reads `pushManager.getSubscription()`
and, if one exists, POSTs it to the subscribe route (`web/src/push.ts`); the
route is an upsert on the endpoint, so a row that exists is rewritten
unchanged and a missing one is restored. Failures are logged and not shown.

The worker also handles `pushsubscriptionchange`: it subscribes again with the
old subscription's `applicationServerKey` (or the key route when the browser did
not keep the old one), POSTs the new subscription, and DELETEs the old
endpoint. The request is same-origin, so the session cookie authenticates it.
A worker has no page to read its control base from; it takes `/sessions/<id>`
from an open window's URL and otherwise posts at the root, which a host does not
serve — the next app open then repairs it.

The worker is registered on the dev path too (`/sw.js?dev=1`, which caches
nothing; [web-client.md](web-client.md)), so a dev-proxy host can hold
subscriptions. The checkbox has four states: pending, subscribed, unsubscribed,
and unavailable — no worker became ready within a few seconds — which renders
"no service worker on this page" beside the disabled box.

## No push while someone is attending

A notification about a moment the operator is already at the screen for is noise, and
one nobody attended to is the point of the feature. The host decides between the two
from what clients report, not from what they have open.

**Attention.** Clients report `{client, session, active}` to the host-level
`POST /attention` ([control-api.md](control-api.md)): `client` a per-page-load id,
`session` the session the page shows (`null` on the session list), `active` whether a
person is attending. The host keeps the latest report per client with its arrival time
(`src/daemon/attention.rs`). A client **lapses** 45 seconds after its last report
(`LAPSE`): the web client heartbeats about every 20 seconds while active, so two missed
beats lapse it, and a client that dies holding `active` stops counting within the
minute with nothing to clean up. Someone is **attentive** when any unlapsed client's
last report was active. The table is host-wide: the host hands one `Arc` to every
session it adopts, so a push for any session reads what every client has said. A
session outside a host has no table and nobody is attentive.

**Escalation.** `Session::push` sends at once when nobody is attentive. Otherwise it
holds the message for `ESCALATION_DELAY` (two minutes, one value for every moment) on
a task on the host's runtime (`src/daemon/escalation.rs`), and sends it if the moment
is still standing when the delay ends. A moment stops standing when it
**resolves** or is **seen**:

- *Resolves:* the gate is decided (`resolve_approval`, whichever decision), the `ask`
  ends (`AskWait` drop), a turn is submitted to the session (`submit_turn`, which also
  carries an `ask`'s reply and ends a settle), or the session is archived (the host's
  teardown).
- *Seen:* an active report arrives from a client whose `session` is this session
  (`Host::report_attention`). An active report requires recent input, so this reads as
  the operator opening that session and doing something. A report for another
  session, an inactive report, and a report that arrived before the moment do not
  cancel it.

Each session holds one pending escalation. A newer moment replaces an older one (the
older timer is aborted), and a moment that sends at once drops a held one: the newer
push names the same session. A timer claims its own slot entry by task identity, so a
cancel that races the delay leaves nothing to fire. A push that fires is therefore one
nobody attended to, and the service worker shows every push it receives.

What breaks if this changes: reading *connected* or *visible* as *attending* silences a
desktop window nobody is at, and any one attentive device silences every device; with
escalation, a wrong attention report costs a two-minute delay and never a missed
notification.

**The web client** (`web/src/attention.ts`) reports `active` when the page is visible,
focused, and has seen pointer, key, wheel, scroll, or touch input within 60 seconds.
It reports at once on a change of `active` or of `session` (client-side navigation
between sessions included), sends an inactive report on hide or blur so the host need
not wait for the lapse, and heartbeats every 20 seconds while active. A change to
active is throttled to one per 10 seconds. A check every 5 seconds notices the input
window expiring, since idleness fires no event. Failures are logged and not shown; a
`404` (a lone session bind) stops reporting. The stream is not tied to attention: a
hidden tab keeps its `/events` connection.

**The TUI** (`src/tui/attention.rs`) reports as a client of its own, with a
per-process id as `client` and the session it is attached to as `session` (`null` on a
lone session bind). It is `active` while its terminal is focused (focus reporting,
`CSI ?1004h`) and a key or mouse event, `Moved` included, arrived within 5 minutes
(`INPUT_WINDOW`): a person reading output without touching anything for longer lapses
to the escalation, which costs a delayed push at worst. Focus starts as focused, and
starting counts as input, since the operator has just launched or attached it; a
terminal or multiplexer that never sends focus events (tmux without `focus-events on`)
leaves it reading focused, so input recency alone decides there. It reports on a change
of `active`, heartbeats every 20 seconds while active, sends an inactive report on
focus loss and on quitting, and checks the input window on the loop's one-second tick.
Switching sessions attaches afresh and the new attach's first report names the new
session. Reports go out in order from a task of their own through the host-root
`POST /attention`, never blocking the UI loop; failures are logged, not drawn, and a
`404` (a lone session bind) stops reporting.
