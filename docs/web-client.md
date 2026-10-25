# The web client

A React single-page app served by the daemon's TCP bind ([web-bind.md](web-bind.md)):
the second human client after the TUI, and the mobile client. Sources live in
`web/` and are built and embedded into the serving binary at compile time
(`trillium-frontend` in `src/daemon/tcp/routes.rs`); there is no separate static host. It is
a client of the same `/v1` surface as everything else ([control-api.md](control-api.md))
— it holds one stream connection and derives everything from its frames, exactly as
the reference TUI does.

## UI development tools

For UI polish and transient-state debugging in a dev build:

- **Component gallery:** open `/?gallery` for fixture-driven components and composed
  transcript states. See [The design gallery](#the-design-gallery--websrcgallery).
- **Stream tape and replay API:** use `window.__scrub` to `hold()`, `step(n)`,
  `seek(n)`, and `resume()` the session's incoming frames. `tapes()` lists saved
  recordings; `restore()` loads an earlier recording. This lets browser automation
  inspect an interim state without racing the live stream. See
  [Scrubbing the stream](#scrubbing-the-stream--websrcsessionrecordts) for semantics,
  persistence, and the DOM-event API.

## Positions (from the plan that gated it)

- **Parity of information, not parity of interface.** The transcript is whole and
  faithful: every log entry renders, because a gate resolved over a snippet is not an
  informed choice. What it does not copy is the TUI's furniture — control strip, focus
  model, block menu — which is form-factor idiom, not shared invariant.
- **A tool result's primary view is what the model saw.** The selection is the
  output; raw is the audit view, one tap away. Rendering that view client-side
  runs through the cordon (below) — decided, deletable debt, not the "shared
  home" a promoted UI finding normally gets. The long-term shared home is
  structured tool output (audience-tagged content blocks, or per-tool result
  schemas), which deletes the cordon whole; the reserved `render` field on log
  frames remains the daemon-side alternative if structure is slow to arrive.
- **Content before styling.** This gated the first slice; with the information
  surface substantially built, styling now arrives from the design pass,
  starting with the theme layer below.

## The design source

The visual design lives in the claude.ai/design project "App design iteration"
(`https://claude.ai/design/p/b6ea6457-2c10-4529-b17b-ad5eeb18ca9b`).
`Efference Session - After Dark.dc.html` is the composite mock; `Daylight` is its
light counterpart; per-region iterations (Session View / Transcript / Inspector /
Bottom Panel / File View) and the brand-identity uploads sit beside it (including
`efference-lowcontrast.tmTheme`, whose provenance is unconfirmed — it may be the TUI
asset).

How to read the mock (jbr, 2026-08-19): it was drawn without visibility into the
client's code, so the **layout and IA are the signal**; the fine detail is imagined
and gets filled in during implementation, not treated as requirements. Concretely:
specific keyboard binds are deferred entirely — the layouts reserve hint surfaces,
and jbr drives concrete binds through individual suggestions once the pass is up;
the top bar starts minimal and grows interactively; invented metrics (batch timing)
are dropped. A departure from pure restyling gets raised for discussion, not taken
as marching orders — the discussion outcomes so far are recorded in each unit.

## Layering — the churn gradient

The tree is ordered by expected rate of change, so display iteration never touches
the wire:

| layer | job | React? |
| --- | --- | --- |
| `web/src/api/` | mirrored `/v1` types (`types.ts`), the control base (`base.ts`), the page's bearer (`bearer.ts`), the one request entry point `apiFetch` + the fetch wrapper + error envelope (`http.ts`), the SSE reader (`stream.ts`), the `/files` reads and change-poke streams (`files.ts`), the host's own routes — the listing stream (`sessions.ts`) and the new-session surface (`sessionForm.ts`) | no |
| `web/src/session/` | the fold: frames → an immutable `Session` snapshot (`store.ts`), the connect lifecycle (`connect.ts`), session verbs whose error recovery folds into the store (`actions.ts`), one `useSyncExternalStore` binding (`useSession.ts`) | binding only |
| `web/src/ui/` | display — transcript, login, composer; the churn zone | yes |

`types.ts` is the one place wire shapes are declared client-side; nothing in `ui/`
names a wire field. The fold in `store.ts` is pure functions over plain data —
`applyFrame(session, frame)` — with vitest coverage of the properties the API doc
promises (idempotent log replay, whole-topic state replacement, buffer settlement
when an entry commits). When the control API moves, `api/` and the fold are the
whole blast radius.

The fold treats `output` as **lossy and unordered relative to `log`**, because it
is: the daemon drains an available log entry before reading output deltas
(`Tail::next_frame`), so an `Assistant` entry can be emitted ahead of deltas
still queued that describe it, and those arrive after the commit cleared the
pre-commit buffers. Folding them opened fresh blocks holding the tail of what
had just committed — a call block with no name (only `tool_call_start` carries
one) whose input was a trailing `}`, or the message's last sentence a second
time — which then survived until the *next* entry committed, a whole
tool execution later, reading as "it resolved itself". So an `Assistant`
commit puts the fold in a `draining` state and block deltas are dropped until
the next log entry ends it; an assistant response never directly follows another,
so the drain cannot swallow a real one. Tool and process output are unaffected —
they are keyed by call and process, not scoped to a turn.

Side-model output uses separate capped buffers (`session/side-stream.ts`): compaction
and detached consolidation each retain account text and thinking, and triage retains
thinking and verdict text per normalized set of call ids (an empty set is idle triage).
These do not enter the main assistant blocks or their drain. Request stages and
`current_operations` fence late output; a successful end retains text already displayed
until its durable Summary or verdict arrives, while failure, retry, resume, or a new
request resets it. The consolidation checking entry supplies its frozen `from`/`upto`
span before the request starts. Summary, terminal consolidation telemetry, Triage, and
ResultVerdict entries settle the corresponding buffers. Because log/state delivery can
overtake queued output and these deltas carry no request id, a fully overtaken stream
is omitted in favor of its durable record rather than reconstructed.

The split carries a second load beyond rate of change. This layout targets the
desktop browser, and the phone is expected to need drill-down views rather than a
reflow of it — but it is one app meeting both, over the same screens. So fetching,
folding, and derivation stay in `api/`, `session/`, and hooks rather than inside
the components that arrange them, and meeting the phone stays a **layout** change
rather than a second implementation. Wiring logic into a desktop-shaped component
is what the split forbids; the phone breakpoint below is what the licence bought.

The binding (`useSession.ts`) owns the store in a memo but the connector in the
effect: connecting is a side effect, and `Connector::close()` is permanent, so
React StrictMode's mount → cleanup → remount cycle (dev builds only) must build
a fresh connector on remount — the fold's idempotent replay is what makes the
re-seed harmless. The hook hands out a stable facade over whichever connector
is currently live.

The stream reader is fetch + a hand-rolled SSE parser rather than `EventSource`:
the daemon sets `event:` to the frame's kind, and `EventSource` dispatches named
events — a client would register one listener per known kind and silently drop
frames from a newer daemon. Reconnection resumes by `?since=` (the daemon does not
read `Last-Event-ID`), replaying the durable subset exactly.

Every request the client makes — reads, verbs, streams, and the bytes of images —
goes through one function, `apiFetch` in `api/http.ts`, which calls the global
`fetch` with the held bearer — this browser's device token — attached as
`Authorization: Bearer` ([web-bind.md](web-bind.md)): merged into whatever headers the
caller passed, never replacing an `Authorization` the caller set, and only for the
page's own origin. The
bearer is kept in IndexedDB with one in-memory copy (`api/bearer.ts`); `main.tsx`
reads storage before anything that makes a request starts, so attaching the header
needs no wait, and storage that is missing or throws leaves the in-memory copy
working until the page reloads. `web/eslint.config.js` bans the `fetch` and
`EventSource` globals in `web/src` (`no-restricted-globals`), and the same names as
properties of `window`, `globalThis`, and `self` (`no-restricted-properties`), with
`fetch` allowed in `http.ts` alone, so a bare call fails lint. The rules apply to
tests as well; they stub the global by name (`vi.stubGlobal("fetch", …)`), which
they do not flag, and `apiFetch` reads `fetch` at call time, so the stub is what
runs. The service worker (`public/sw.js`) sits outside `src` and calls its own
`fetch`.

**A refused bearer is decided in `apiFetch`.** A `401` to a request that carried the
held bearer clears it, and only that one: a bearer stored since the request left is a
newer sign-in, and a request that brought its own `Authorization` says nothing about the
held one, so a late `401` leaves a fresh sign-in alone. The bearer store announces each
change (`subscribe`), and the top of the app follows it: while none is held `useEntry`
answers `unauthenticated` and `App` renders the sign-in view in place of whichever view
was showing — the list, a session, the archived-sessions page, the file sidebar — so a
`401` does not reach a view's own error text. The replaced view unmounts and closes what
it held (the listing stream, a session's connection, its file streams), which is why
none reconnects against the dead bearer; the attention reporter (`attention.ts`), which
no view owns, sends nothing while none is held and checks for a due report as soon as
one is stored. Storing a bearer mounts the page's view afresh: a session catches up and
opens its stream from its warm store's cursor, and the list opens a new listing stream.

The catch-up read and stream opener retain structured HTTP failures. A `404` marks
the session `missing` and stops
retries. `SessionView` replaces a missing host session's page with the root,
without a toast, so Back does not return to the absent session. A known handoff
successor takes precedence over going home. Network failures and other HTTP
errors stay on the reconnect path rather than triggering navigation — a `401`
(the paragraph above) and the `409
starting` a starting session answers the catch-up read with among them, so its
stream opens from zero. A stream that ends after a failed start (the `starting`
topic carries a `failure`) marks the connection `ended` and stops retries: nothing
is behind it to reconnect to (*A session that is starting*, below).

### Earlier transcript windows

`connect.ts` opens with an 80-entry tail; `history.ts` fetches 80-entry backward
pages with `before` and `tail`. Counts are durable log entries, including records
that do not render as conversation. `EarlierHistory` observes the row above the
transcript with a 300-pixel top margin. It loads when the reader approaches that
row after leaving the tail, or when a short initial window cannot fill the
viewport. A failed page exposes Retry instead of automatically repeating the
request; a short page marks the beginning of the session.

The loader belongs to the warm store and binds every read to its session base.
Only one backward request runs at a time. A response whose starting boundary has
changed is discarded. `prependHistory` merges by sequence, preferring held entries
on overlap; it does not change the live cursor, topics, pending submissions, or
pre-commit output. The existing stream continues while pages are fetched.

A cut can leave a result, spot, telemetry record, or result verdict without its
owner. `coherentWindow` resolves the issuing call sequence or result sequence and
reads the intervening range, repeating until those tree relationships are within
the window. This can extend a page beyond its requested count. Keeping the range
contiguous prevents a dereferenced earlier call from moving the paging cursor
past unseen entries. The initial tail uses the same repair. If that repair fails,
the fetched tail and its cursor remain usable; a missing call reference is not
classified as a missing session.

Transcript item roots carry stable node keys in the DOM. Immediately before a
page merges, the surface captures the first node intersecting the viewport; its
layout effect restores that node's previous offset after the prepend. It adjusts
only residual movement, so browser scroll anchoring is not applied twice. The
dev recorder incorporates fetched historical prefixes into its replay base when
seeking backward or saving, without treating a history fetch as a live frame.

### Coalescing the fold

A long block arrives as thousands of `output` deltas, and the pre-commit tail
that displays it re-parses the whole block as markdown when it renders. That
cost is linear in block length while the delta rate is roughly constant, so the
work over one block is quadratic: measured at ~8 ms per render of a 10 kB block
in the suite's environment, past a delta's arrival interval well before a block
of that size finishes. Once the folding thread is behind, the deltas already
buffered in the fetch body keep it saturated after the producer has gone quiet —
so the page did not recover when the stream ended, only when it was reloaded
([../journal/2026-08-27-web-streaming-freeze.md](../journal/2026-08-27-web-streaming-freeze.md)).

Two mechanisms bound that, both below `ui/`:

- **`Store` separates folding from notifying.** The fold stays synchronous —
  `snapshot()` is current the instant `update` returns, which is what the resume
  cursor, the scrubber's refold, and the process seeder read back — but
  subscribers are told at most once per animation frame. A backlog of any size
  collapses into one render at the current length, so outstanding work is bounded
  by the frame rate rather than by the delta count. `flush()` notifies
  immediately for a caller that needs the render to have happened: the scrubber
  uses it while held, so `step()` from the console lands before the call returns,
  and stays batched while following.
- **The dispatch loop yields to the task queue.** Every step of `openStream`'s
  `for await` is a promise continuation, and `sseData` yields each event parsed
  out of one read without awaiting anything else, so a producer faster than the
  consumer holds the thread against timers, input, paint — and against the
  animation frame the coalescing flushes on, which would make the batching
  unobservable. The loop gives the task queue a turn whenever 8 ms of wall clock
  have passed since the last one, via a `MessageChannel` round trip rather than
  `setTimeout` (whose nested-timer clamp would floor each yield near 4 ms).

Follow-tail (`ui/useFollowTail.ts`) scrolls after every render, so batching moves
it from once per delta to once per frame — the scroll now lands on the frame that
painted the growth rather than repeatedly within it.

Two costs the batching bounds but does not remove: the growing block's markdown
is still re-parsed whole on each of those frames, and a fenced block inside
still-streaming prose is re-tokenized whole (`Code`, not `StreamingCode`).
Memoizing the parsed prefix and deferring highlight for streaming content are the
remaining reductions.

## Which session this page is, and where its API lives

The client treats its control endpoint as an opaque base URL and joins every path
onto it ([control-api.md](control-api.md)). `api/base.ts` derives that base from
the page's own location, because the page was served *from* its base: a path under
`/sessions/<id>/` yields `/sessions/<id>`, anything else yields `""`. A session
daemon at a socket root and a host at a prefix are then the same client code, and
nothing above `api/` names a binding. No host serves the page today; the session
list view and the prefixed base are held for the in-process host
(`backlog/inbox/host-process.md`). `/auth/…` and
`/sessions` are deliberately *not* joined: they are the listener's own surfaces,
at its root under either binding — one credential covers every session behind a
host.

The id set the derivation accepts mirrors the gate's (`plain_id`, `src/daemon/tcp/gate.rs`):
a path whose second segment is not a plain segment is not a prefix, and addresses
the root instead.

**What the root shows is a probe, not a configuration.** With an empty base the
binding is genuinely ambiguous — the shell is byte-identical — so `session/entry.ts`
asks: the host answers `GET /sessions` with the listing, a session daemon serves no
such route and answers `404`, and a host with no credential yet answers `401`
(the sign-in view). Anything else — offline, a daemon that went away — reads as a
session, which is what a root has always been;
the session view's composer status reports the network state, where a second
failure screen would only restate it. The probe runs again whenever the held bearer
changes, signed out included, and an answer that arrives after the bearer has moved
again is dropped, so at a sign-in the sign-in view stays up until the probe under
the new bearer answers.

**The list is a subscription** (`ui/SessionList.tsx` over `api/sessions.ts`).
`GET /sessions` is one resource with two readings: asked for JSON it answers the
listing, and asked with `Accept: text/event-stream` it answers the listing now and
again on every change ([control-api.md](control-api.md)). The list holds the second
open, so a session created or ended anywhere — another client, a session
commissioning one, a host reaping one — appears or disappears on its own, with no
poll and no reload button. Level-triggered and cursorless: each frame is the whole
listing, so a missed frame is repaired by the next and a reconnect is a fresh
subscription rather than a resume handshake. The probe's answer seeds the first
render; nothing above the list re-resolves after that.

**The root is an overview, grouped into workspaces** (`ui/overview.ts` is the model,
`ui/SessionCard.tsx` a card, `ui/SessionList.tsx` the page). Sessions group by working
directory, one card each, in a stable order — live workspaces by path, cards in
listing order — so urgency is carried by the card rather than by moving it. Up to
five archive-only workspaces follow, ordered by their latest resumable session's
activity (creation time when activity is unknown, then path to break ties). Their
age does not remove the workspace: archived sessions active within one hour appear
as full cards (five per workspace, two on narrow screens), and those within one
week appear as compact rows (up to five); the remaining sessions are available
through the archive picker. Each archived card and row carries the same `⋯` menu as a
live card, with one action: **Switch model…** opens the switch page on that session and
resumes it on the chosen models instead of the recorded ones
([resumption.md](resumption.md)). The row itself stays one button that resumes it.
Roots with only unreadable sessions are not included.
The archive inventory is fetched on mount and when the live listing changes, so
ending a workspace's last live session leaves its archived work available at home.

A workspace heading sets the path's last component as
the name and the parent small and faint (`~/code/` **efference**), because the name is
what the operator recognizes it by and the parent is what tells `~/code/api` from
`~/work/api`. Headings are sticky section headers: each docks under the top bar while
its workspace is on screen and is pushed off by the next. A sticky element is held
inside its section's content box, its own margin included, so all spacing around a
heading lives in the card grid's padding — margin anywhere else opens a gap between one
heading leaving and the next docking. The bar's height is measured into
`--top-bar-height` rather than assumed. While content passes beneath a docked heading
it takes the bar material — translucent, blurred, a hairline — and the bar drops its
own hairline so the two read as one surface; the bar on this page is always that
material. "Docked" is an `IntersectionObserver` (`useStuckHeadings`) because Safari has
no `scroll-state` container query.

**A card says where the operator's attention stands, in words.** `attention` is one of
`blocked` (gated, stalled, parked at the cold check, or blocked: nothing moves until someone acts), `your_turn` (idle after
a turn — waiting on a reply or an end, not "less important"), `working`, and `new`
(never made a request and has no title). The top line is one sentence with the id as
its subject — `amber-lattice needs approval`, `hollow-birch is waiting for you`,
`tidal-cipher has stalled`, `amber-lattice is paused: its cache expired` (status
`parked`; the activity line says `continuing reads 120k tokens uncached`, and after a
compaction `is paused: summarized, waiting for you to continue`), `quiet-fern is held` or
`is stopped: its config file needs repair` (status `blocked`, the reason from the
`loop_phase` topic) — and working and new say nothing, working being the
default. There are no status dots. Colors keep the meanings the session view gives
them: a gate is `--accent` (and tints the card), a stall or a park `--warn`; the
switcher dot follows.

Two voices, three sizes: prose — the title, the status predicate, the activity line —
in the serif; identifiers — id, models, a command, usage — in mono. The title is the
session's ([session-title.md](session-title.md)); a session with turns and no title
yet reads *Untitled*, because titles are written after the first turn rather than with
it, and only a session that has never made a request reads *Ready to get started*.
The activity line is, in order: the gate's tool and command, the stall's message, the
running call's `intent`, or the first paragraph of the last reply as plain text. The
footer is `key: value` facts — model, spotter, triage only when it differs from main,
supervisor, supervises, running processes — then the context meter and cost.

**Supervision is a relation between peers, not nesting.** A session another created
gets its own card like any other, naming its supervisor as a link, and the
supervisor's card lists what it supervises. Working with a subagent is no further away
than working with the session that created it.

**Each card follows its session on one connection** (`openSessionCard`): a cold
`GET /log?tail=12`, then `/v1/events` from just past it carrying the log and the card's
state topics (`gate`, `ask`, `stall`, `loop_phase`, `cold`, `usage`,
`session_identity`, `upstream_retry`); `cold` is what says whether a parked session's
cache is still believed expired, and `loop_phase` why a blocked one is blocked. Glancing at the overview reports nothing: attention is what the
page's own report says ([web-push.md](web-push.md)), not what it has open.
`foldRecent` reads the latest intent and reply off the log; a user entry retires the
reply it answers, and a record-only assistant entry says nothing. That is N
connections and the whole log for a card that reads two fields, accepted while
whether the intent line earns its place is learned (`backlog/inbox/host-overview-stream.md`).

**Ending is the one thing here that is not a navigation**, so it sits behind the card's
menu rather than beside the title. `DELETE /sessions/:id` reaps the session's
background processes and removes its scratch, and the host answers only once that
teardown has run — so the card leaves on the next frame of the listing stream, which is
already out by the time the call returns. The confirm rule is `ui/sessionRow.ts`, the
same rule the TUI's switcher applies ([tui.md](tui.md)): a session that is doing
something, or that holds processes an end would reap, confirms in the menu naming
which; an idle session with nothing running ends on the first click. The menu's other
action is **Switch model…**, which opens the switch page on that session
([resumption.md](resumption.md)): the model change stated as an action on a session
rather than on a resume. The top bar's "New session" and each workspace's `+` open the
new-session form below.

**Entering a session is a client-side navigation, and each session keeps its own state.** A
card's title is an `<a href>` to `/sessions/<id>/`, and the back affordance in the top bar is
the wordmark, a link to `/` only when the base is non-empty. `ui/route.ts` installs one click
listener on the document: a plain primary-button click on an anchor whose target is the list
or a session's own view is a `pushState` and a re-render, and every other click (a modified
one, a `target`, `/tree`, `/auth/…`) is left to the browser — so the links stay real anchors.
`popstate` re-renders the same way. A card is an article whose title link is stretched over it,
so the whole card opens the session while the supervisor links and the menu stay separately
clickable. The one client route besides `/` is `/tree`, and it is `/sessions/<id>/tree` under
a host: the same route with the base in front of it, which is why the route is read with the
base removed.

What makes a switch safe is that a session's base is fixed for the life of its view. The api
layer derives the base from `location` on every call (`api/base.ts`), so `connect` captures it
when a view mounts and passes it to the catch-up read and the stream (`fetchLogTail`,
`openStream`), whose reconnects must reach the session they began on even after the page has
moved. `ui/App.tsx` keys the session view by base, so inspector selection, the open file, the
file tree and the composer's local state are per session, and a pop between
sessions is a remount.

**Warm sessions.** A session's `Store` comes from `session/registry.ts`, which retains the
stores of the last `WARM_SESSIONS` sessions viewed. Leaving a session closes its connection
and keeps its snapshot; returning renders that snapshot on the first frame, and the connection
resumes from the cursor — a tail read merged into what the store holds (`reseed`, so the
scrolled-through window is not replaced) and the stream from `lastSeq + 1`. A retained store
has no connection of its own: an open stream would count as someone watching and suppress that
session's push notifications (`web-push.md`), so a session that is not on screen costs nothing.
A non-live connection occupies the composer status slot rather than shifting the transcript.
The first visit to a session in a page's lifetime
is cold unless its tab was hovered or focused first: that starts one tail read
(`prefetchStore`, no stream, applied only to a store nobody has touched since, so a late read
cannot move a cursor back) into a small separate pool, from which a visit promotes it; without
one, the store starts empty and fills from the tail read on arrival. The switcher's tab row and the
header's title are seeded from the last listing any view heard (`session/listing.ts`) rather
than from the session's own `session_identity` topic, which a cold store does not have yet.

History: the overlay entries (`ui/overlayHistory.ts`) belong to the view that pushed them. A
view's pop handler stands down once the page is on another session's base, and a view that
mounts onto an overlay entry an earlier view left consumes it with one `history.back()`, so
Back from a session switched to with an overlay open lands on the previous page rather than a
phantom overlay. The new-session and resume pages (`?new=`, `?resume=`) belong to the list and
are unmounted with it, and so does the switch page when it opens from an archived row:
`?switch=model&id=<id>`. On a session's own URL the same page is `?switch=model`, which
keeps the session view mounted behind it.

`ui/App.tsx` is only this choice — list, login, or session. The session view is
`ui/SessionView.tsx`, which is the component the rest of this document means when it
says App holds a piece of state (the inspector selection, the open file request, the
overlay count).

`ui/AppHeader.tsx` supplies the shared header frame and wordmark navigation.
`TopBar.tsx` supplies session identity, usage and the app-settings link; the
overview and launch pages supply their own actions. The wordmark links to the session
list, with a callback available for draft-preserving navigation within the launch
flow. Modified clicks keep native link behavior. A page without a list can pass
a null home URL to render a non-link wordmark. Caller refs attach to the actual
header so overview docking still measures its rendered height.

## Switching a session's models

The switch page (`ui/SwitchModelPage.tsx`) is a resume patch in the client's words: a
live session is archived and then resumed on the models chosen, a dormant one only resumed
([resumption.md](resumption.md)). It opens as a query stage on the session's own URL
(`?switch=model`), so the view behind it stays mounted, and from an archived row as
`?switch=model&id=<id>`, which only resumes.

Two details are the client's, not the form's:

- **Which root the patch names.** The resume names the root the host serves the session
  under, which is the listing row's `cwd` (`session/listing.ts`), not the identity topic's
  — that one is the working directory the session runs in, its own worktree included. A
  patch naming the working directory answers `409 state_mismatch` and leaves the session
  archived, so the view of it goes 404 and the client reads that as a session that ended.
  `ui/liveSessions.ts` is the listing subscription both the switcher's tabs and a session
  view read for their own row; `api/sessions.ts` serves every reader from one stream.
- **Standing still across the restart.** Archiving closes the log, and the resume claims
  the id again only after that, so the view behind the page reads a 404 in between. It is
  the gap, not the end: while the page is up, and until the resumed log's `starting` topic
  arrives (`session/store.ts`), `SessionView` does not take a 404 as its cue to go home. The
  starting session's own status line then carries the restart's progress, and the
  post-start reattach brings the view back from its cursor.

## The new-session form

The overview's **New session** and workspace **+** open full-page location,
settings, and role-specific model selection. Query-backed routes participate in
browser history; the overview owns the raw override draft. The implementation,
preview/creation guards, model hierarchy and touch behavior are documented in
[Session creation in the web client](web-session-creation.md).

## The phone breakpoint

An interim usability pass (2026-08-22), not the mobile design pass the backlog
scopes — the minimum that makes the desktop layout survivable on a ~400pt
viewport, iterated live in the iOS simulator. One width breakpoint, 700px,
declared twice on purpose: the `@media (max-width: 700px)` block at the end of
`index.css`, and `ui/useNarrow.ts` (a `matchMedia` binding, the one JS-side
reading) for what CSS cannot switch. Change both together.

This pass scopes the layout, not the phone. The desktop browser is the target;
mobile is supported by a reflow of it, and phone-layout defects in that reflow
are ordinary backlog units. What remains open is whether a phone eventually wants
more than a reflow: fitting a harness's worth of information onto a phone may
need drill-down views that depart far enough from the desktop layout to be their
own wireframing pass, which has not run
([../backlog/inbox/web-mobile-inspection-design.md](../backlog/inbox/web-mobile-inspection-design.md)
is that unit). The breakpoint's cheapest workable moves — terse results with
tap-to-detail, the inspector as a full-screen sheet — already landed on that
drill-down shape, which is evidence for the prediction, not a substitute for the
pass.

Functionality is a different question, and it is not scoped out. This is one app
that will meet the phone case too, and a *capability* motivated by using the
harness away from the machine is in scope. The standing constraint on how units
are built follows from it and is stated above under layering: state machinery
stays out of the components, so meeting the phone later is a **layout change**
rather than a second implementation. The free pass is on making it responsive
now — not on wiring logic into a desktop-shaped component.

What narrow changes:

- **The label gutter stacks.** The entry grid drops to one column — label above
  content, left-aligned — because a 6rem gutter plus insets is ~40% of a phone's
  width. The user turn's accent border is on the content, so it survives.
- **Tool results expand inline** at both widths: up to 20 retained lines show
  whole; longer selections initially show ten. Omission rows reveal captured
  context or additional retained lines. Call arguments still truncate at 12
  lines on desktop and 4 on narrow screens, with the inspector holding the whole.
  The argument marker adds `· tap to inspect` on narrow screens because touch
  has no hover to discover the affordance with.
- **The inspector is a full-width sheet** (`100vw` overriding the persisted
  inline width; the resize grip hides — it has no touch meaning).
- **The composer band collapses when unfocused** to reclaim the dock's third of
  the screen: a floating `✎ compose` chip (accent-bordered; labelled `draft…`
  when one is held) summons the band and focuses the field, `⌄` or Escape or a
  successful send puts it away, and the draft survives collapse. The chip
  yields to a pending gate — the float owns the above-dock slot and carries its
  own steering input.
- **The composer stops advertising the keyboard**: placeholder drops the
  enter/shift-enter advice, Enter inserts a newline instead of sending (a phone
  has no shift-enter, so intercepting Enter would make multi-line drafts
  impossible), and the send button sends.
- Chrome rows tighten: the context meter narrows, the title drops from the top
  bar, the upstream chip ellipsizes, and the gate's decision rows wrap so every decision stays on screen (verified
  live: a `human_only` gate decided by tap from the simulator).

**Overlays are history-backed** (`ui/overlayHistory.ts`, both form factors):
each inspection depth and lightbox — owns one history entry, so the
back button and Safari's edge swipe (which *is* history-back) dismiss the
overlay instead of leaving the app. Not a router: the overlays remain App
state, and the hook *reconciles* — App reports how many overlays are open, the
hook pushes or consumes entries to match — so every existing close path (✕,
Escape, tapping the inspected call again) converges without touching history
itself. Forward navigation onto a consumed overlay entry is re-consumed:
which overlay it stood for is deliberately not recorded.

One WebKit finding the composer carries, fixed for all widths because it only
*manifested* narrow: the textarea's flex basis is `100%` rather than `flex: 1`'s
`0%` (WebKit's `field-sizing: content` shrink-wraps the inline axis when the
basis is indefinite, which collapsed the field to ~1ch).

**Nothing in the transcript may set a minimum width wider than the column.** Two
failures of this shape were found together at narrow widths and fixed the same
way. A call head is a wrapping flex row whose signature holds a `min-width`
floor, because without one, shrink took a multi-path `read` target down to a
one-character-per-line column beside squeezed basename links — 2673px of height
for one head. A triager note and markdown prose break inside long tokens
(`overflow-wrap: break-word`), and the note additionally sets `min-width: 0`:
as a flex item its default `auto` floor is the longest identifier it contains,
so a note full of them — which is what a note about code is — grew its box past
the row and had every line-end cut at the screen edge. Both halves are needed
there; `min-width` alone lets the box shrink while the unbreakable token still
paints past it.

**The jump-to-latest chip** (`ui/JumpToLatest.tsx`, both form factors): a fixed
chip above the dock, shown only while follow intent is disengaged. Tapping it
explicitly resumes following and jumps instantly through `jumpToTail` in
`ui/float.ts`; `useDockOffset` keeps it above the measured dock. Hidden while a
gate is pending (the float
owns that slot and carries its own jump) and while a current-file sheet covers the
transcript below the desktop sibling breakpoint.

Known nit: wire-vocabulary glyphs quoted in an *inline-code span* (a model
writing `` `⟢` `` in its own prose) render as tofu on iOS — IBM Plex Mono lacks
U+27E2 and the iOS mono fallbacks don't fill it; the serif covers it, as does
the renderer's own `✧` note chrome (backlog/parked/icebox.md holds the fix candidates).

## The cordon — `web/src/render/`

A deliberate, temporary duplicate of daemon and TUI rendering logic, kept in
one directory so it can be deleted whole when structured tool output lands.
Two residents:

- `triage.ts` — a literal port of `src/project/render.rs` (the verdict
  renderer: keep-ranges resolution, `@@` hunks, the `⟢` note, the truncation
  breadcrumb, triage-failure and elision texts), tests mirroring
  `src/project/test/triaged.rs` case for case.
- `parse.ts` — a literal port of the TUI's per-tool parsing and dispatch
  (`src/tui/projection/tools.rs` + the per-wire-state match in
  `src/tui/projection.rs`): the `===DIFF===` classification for the edit
  tools, `read`'s sentinel stripping and extension→language map, the call
  signature below with the `intent` leading the entry, and the triaged
  composition — the parse runs over the whole raw, the verdict's kept line
  numbers filter the rows it produced, so a triaged view keeps its tool's
  rendering. The `kept N of M lines` header takes its numbers from the entry
  view's `selection` field when the daemon sent one
  ([control-api.md](control-api.md)) — the local count is over whatever
  content is on hand, which for an elided result is a prefix that may hold
  none of the kept lines; the body then says where they are instead of
  presenting as a verdict that kept nothing. The local count is the fallback
  for a daemon that predates the field. When nothing was dropped
  both header words go quiet (`stateWord` and `sizeLabel` null) and the head
  is a bare `result` — the inspector's wire row still names the state.

`ui/ToolBlocks.tsx` is the display half: colored diff lines (line-level
only), numbered gutters whose jumps show what triage dropped, `data-lang` on
code bodies as the syntax-highlighter's hook, and — since the inspector —
no per-result raw `<details>`: raw is the inspector's tab, from the entry
the client already holds. Ports here
stay literal (same names, same strings, same classifications) and never grow
beyond mirrors: a rendering idea that isn't a mirror belongs to the
structured-output successor, not the cordon.

## The transcript hierarchy

`session/tree.ts` projects the flat log into the containment the display
renders — a pure `entries → tree` function beside the fold, not grouping logic
inside components, so it is testable as data. It is to this client what
`tui/projection.rs` is to the TUI: a second projection of the same log to a
different target.

```
session
├─ user text / thinking / agent text / mark / ambient / config-edit / title change
└─ batch                    the calls issued in one step
   └─ call                  one call, whole
      ├─ intent, args
      ├─ spot               the spotter's stance + reason
      ├─ telemetry          duration, outcome, raw result size
      └─ result             carries its own keep + note
```

An assistant entry reaches the tree through `session/assistant.ts`, which groups
its ordered, dialect-tagged `output` (Messages blocks or Responses items,
[log-and-projection.md](log-and-projection.md)) into the thinking / prose / calls
view the leaves and the call nodes render — the client-side counterpart of the
`AssistantOutput` accessors in `src/log.rs`. A Responses `FunctionCall` carries
its arguments as the provider's JSON string; the view parses it into the call's
`input`, and an unparseable string is kept under an `arguments` key so the
call still has a node for its result to attach to. Replay credentials
(a thinking signature, `encrypted_content`) are not part of the view. Nothing in
`ui/` reads `output` directly.

Two containers, `batch` and `call`; everything else is a leaf sibling at session
level. The batch is structure, not display: the transcript flattens it, so two
calls issued in one generation read as two steps (see "Detail by recency"). A result, a verdict, and a call's telemetry are attached to the call they
name rather than emitted where they fall in the log — that attachment is the
point, and it is what makes a result *belong to* its call instead of merely
following it.

`ToolTelemetry` ([tools.md](tools.md)) joins by `call_id` exactly as `Spot` does,
and for the same reason: it is a fact *about* a call, not an entry to read. The
default for a payload the tree does not recognise is a leaf rendered as `unknown
entry`, which is the right default — a new variant is visible rather than
silently dropped — but it means every additive log variant needs a decision here.
`ToolTelemetry` shipped without one and put a raw JSON block after every call.

A result captured before its triage decides (`wire: "Pending"`,
[triage.md](triage.md) "Capture, then verdict") renders its raw
output at once, in the pinned bounded box a running command's tail uses, under a
`result (awaiting triage)` head. The `ResultVerdict` entry that decides it is
folded in `buildTree` onto the slot of the result it names — the slot's entry
becomes a copy of the `ToolResult` carrying the verdict's wire and line counts,
at the result's own seq — so the slot re-renders as the triaged result and the
verdict is never an item of its own. The first non-Pending verdict for a Pending
result decides it; a verdict naming a result that was not Pending is ignored. The
fold runs over the entry list, so the initial window and live frames take the
same path; results logged directly with their final wire render as before.

A result's images render below its body as buttons over `<img>` elements for
`GET /v1/log/{seq}/images/{idx}` ([control-api.md](control-api.md)): the entry
view carries only `{media_type, width, height}` — the attributes that hold the
aspect box before the bytes land — and the bytes arrive through `useApiImage`
(below). Capped at 420px display width; tapping one opens it full screen in the
lightbox. The inspector's result tab carries the same images, so a
picture-returning call is not a drawer of everything but the picture.

**API images are fetched, not linked.** An `<img src>` makes a request no client
code touches, so an image that addresses the API — a result's picture, an
attachment's thumbnail, what the lightbox is handed — is fetched through `apiFetch`,
turned into a blob, and shown from its object URL by `useApiImage`
(`ui/useApiImage.ts`). The component that renders the `<img>` calls the hook with
the API URL; a component that only passes the URL on (to `openLightbox`, into a
draft's `preview`) holds nothing. The cache is module-level and keyed by URL: the
first holder starts the request, holders that arrive while it is in flight share it,
and the last one to unmount aborts what is still in flight, revokes the object URL,
and deletes the entry. That departure takes effect once the effects of its commit
have run, so a holder that leaves and one that arrives in the same commit — a
pending turn's echo replaced by the committed turn — share one entry, and a holder
that arrives once the bytes have landed draws them on its first render. The URLs
address bytes that do not change — a log entry's image, an uploaded attachment — so
an entry is never refreshed; a failed request leaves the cache at once, so the next
holder asks again. The hook returns `null` while the request is in flight and after
a failure; the `<img>` is rendered without a `src` then, so its `width`/`height`
attributes still hold the aspect box, and with an empty `alt` (`imageAttrs`),
because a non-empty one would draw the text under a broken-image icon and lay the
element out at the `width` attribute, past the column. The lightbox calls the hook
on the URL it was opened with, so it holds its own reference: the thumbnail that
opened it can unmount without revoking what the viewer shows. A `blob:` URL is
already local bytes — a staged file's own preview — and passes through unchanged. A
result's picture asks only once its box has come within 1500px above or below the
viewport (`useNearViewport`, an `IntersectionObserver` that stands in for
`loading="lazy"`, which defers nothing once the bytes come through `fetch`), and
keeps what it fetched after that; attachment thumbnails and the lightbox ask when
they mount.

**The lightbox** (`ui/Lightbox.tsx`, store in `ui/lightbox.ts`) is the client's
one full-screen image viewer, mounted once — `LightboxHost` — by the session view
and the gallery. Its state is a module store rather than threaded props because
an image opens from unrelated places (a transcript result, the inspector, a sent
attachment) and the viewer is a single instance. Pinch, wheel, and double-tap
zoom (`ui/zoom.ts`: a scale about the image's center plus a translation, anchored
so the point under the fingers stays under them, and clamped to the stage); a
one-finger drag pans while zoomed and *dismisses* at fit, in any direction
(`ui/swipe.ts`); the backdrop, the ✕, and Escape close it. The stage claims the
gesture (`touch-action: none`), the transform is set inline, and the overlay sits
above the whole z-index ladder because it is the topmost surface. It is counted
in the overlay history, so back and edge-swipe dismiss it first. The stage's
pointer capture retargets the pointerup and click to the stage, so the handlers
read the element the pointer landed on, recorded at pointerdown, rather than the
event's own target.

An oversized result's **text** arrives the same way — a prefix plus
`content_elided`, whole at `GET /v1/log/{seq}` ([control-api.md](control-api.md)).
`resultRender` turns the tag into the head's `16 KiB of 256 KiB shown` marker
(the same words the TUI header uses), and `useWholeResult` (`ui/wholeResult.ts`)
does the dereference: in the transcript it waits for a click, since most
truncated results are scrolled past and fetching them all would restore the
head-of-line problem the elision removed; in the inspector it fires on open,
because opening the inspector *is* the request for the whole thing. A failed
fetch is a value — the prefix stays on screen and the affordance says the rest
did not arrive.

**What is deliberately not a container.** There is no per-user-message grouping.
An operator interjection is often incidental (*"check this path: …"*, an API
tip), and mid-trajectory injection ([control-channel.md](control-channel.md))
makes those normal rather than rare, so hanging subsequent calls off the message
that preceded them would encode a causality that is usually absent. `step` — one
model request — is a real boundary that is computed but not nested on: nesting
would put agent prose a level deeper than the calls issued in the same breath,
and the prose is frequently the answer. The cost, stated so it is not
rediscovered: per-step cost accounting has no node to hang on.

**Vocabulary**, mostly already the codebase's: `call` (one call, whole), `batch`
(the fork's word for the calls of one step), `step`
([control-channel.md](control-channel.md)'s "between-step juncture"), `block`
(a leaf). `turn` is avoided in this vocabulary — it already means both a human
message (`POST /turns`) and a model request. Choosing `call` for the container
reverses [tui.md](tui.md)'s "a call and its result are separate entries"; that
is flat-model reasoning, and the TUI still reads that way.

**Authorship is structural first.** A user turn carries an accent gutter; an
agent turn is the unmarked default voice. Every session-level entry renders as
a two-column row — a label gutter beside the content — and the design pass put
a `you` label in that gutter beside the accent (2026-08-19, held lightly: it
reads as cohesive with the labelled column, reversing the first slice's
label-free user turn, and may reverse back after use). A turn another agent sent
names its sender in that gutter instead (`senderName`, the transcript's `sender`
rule): the session's id for `session[<id>]`, the client's name for `mcp[<name>]`,
with the whole `source` as the label's tooltip and an `agent` class on the row; the
operator's own clients (no source, or `web`) read `you`. Non-voice entries keep
their labels, because those genuinely need naming — except calls and thinking,
which carry their voice by type (a call's intent is its title; thinking is
italic) and sit in the prose column without a label. The earlier failure mode —
a noun *above* every block making every entry look alike — is what the gutter
avoids: labels sit beside content, not between entries.

`ToolTelemetry` is not rendered as an entry either, but unlike `Triage` it is
*kept*: the inspector's "what the call cost" panel states outcome, dispatch
duration, and raw result bytes, with the bash and edit sub-structs appended when
the call carried them. It sits above "what the fork returned" because the two
read as a pair — the raw size the call produced, then what the fork did to it.
Dispatch duration excludes the permission cascade, so a call that waited at a
gate does not read as a slow call.

`Triage` is not rendered. Its one visible fact — how much of the output survived
— the result header already states as `kept N of M lines`, so a `keep: all` line
after every result said nothing the reader did not have. Its batch-scoped fields
(`inbox`, `retries`) have no surface yet; when they get one it is the batch
node's.

**Thinking is a first-class sibling**, not merely rendered. Reasoning is where a
misunderstanding is visible before it becomes a wrong call, so reading it is one
of the better moments to redirect — an argument about prominence, not just
inclusion.

The live thinking tail renders in a box of fixed height (about a dozen lines)
pinned to its newest line: while a long think is the thing happening it is
worth reading — whether a ten-minute pause is going somewhere — but it should
scroll inside itself rather than push the page.

The pre-commit tail renders flat rather than inside a batch: nothing there has
committed, so its calls have no results to contain and no verdicts to carry, and
the committed entry supersedes the whole set at once.

## Detail by recency

`ui/transcriptLevels.ts` holds the rules and `ui/TranscriptItems.tsx` draws
them. The transcript is a run of items — a user turn, a paragraph run, a
thinking block, one call — each given a level by its distance from the tail:
the last 6 render **full**, the next 14 **trimmed**, and anything older
**brief**. Recent work reads whole while it is the thing being followed; the
page's length and DOM weight stay bounded as a session grows.

- **A call**: full is the whole call; trimmed caps it at 20rem under a short,
  steep fade with a centred "Show the whole result" set on the fade (shown only
  when something is actually hidden), the triager's note lifted out below the
  cap; brief is one row — the intent as its title, the tool and what it acted
  on (its string arguments) beneath, the note under that.
- **Thinking**: trimmed is a three-line preview under the same fade; brief is a
  "Thought · N words" pill. Thinking of 120 words or fewer is never folded —
  the median block is about that size, and folding it costs a click and saves
  nothing (backlog/inbox/web-thinking-autocollapse.md).
- **Always whole, at any distance**: user turns and assistant prose; a call
  still running or held at a triage; a failed call; a result withheld by a triage
  failure; a call the spotter gated or watched. Errors, gates, and refusals
  render at every density.

Every fold opens to the whole on a tap, so this is folding by position rather
than dropping: every entry still renders, as the positions require, and a gate
decides a call, never a result, so no pending decision rests on a folded body.
Levels are not yet anchored against the scroll: folding an item above the
viewport while the reader is scrolled up moves the page — harmless while
following the tail, and the open item in backlog/inbox/web-thinking-autocollapse.md.

**A call's parts, top to bottom**: the intent as its title (upright serif,
inline markdown), the signature under it at a smaller size, then what came
back — the result header (interface text, in the sans), the body, and the note
— indented as one section under the question. The `›` prompt and the `✧` note
glyph are not drawn: they are wire vocabulary with no meaning to a reader here.
Items are separated by space alone (3rem), with tight spacing inside one, so a
call's parts group by proximity; the hover is a padded field over the whole
item, note included.

## A call reads as a signature

Every call renders as pseudocode. `ui/signature.ts`'s `callView` decides what the
arguments are, width-free; `ui/signatureLayout.ts` lays them out at the column's
width; `ui/CallHead.tsx` renders the rows:

```
ferritin.search(crate: "serde", query: "Deserialize")
check_inbox()
read("src/api.rs", "src/boot.rs", from_line: 3)
write("src/main.rs", overwrite: true)
  contents
  ⋯ the file, highlighted in its language
```

The form is **total**: MCP servers are user-mountable and their argument shapes are
arbitrary, so a tool this client has never heard of renders as well as a built-in.
It replaced a per-tool table plus a pretty-printed JSON envelope for everything not in
it — which is how `check_inbox` came to render as a code block containing `{}`.

- **The name is the dotted one.** `<server>.<tool>`, never the wire's
  `<server>__<tool>`: the dotted form is the only spelling an operator ever
  writes (an eval config's `tools = ["ferritin.get"]`), so showing the wire form
  showed a name that appears nowhere else in the product. `splitToolName`
  mirrors `mcp::display_name` (the boundary is the *first* `__`) and is what
  `ui/policy.ts` dots the policy tab's grants with too. The server segment is a
  dimmed span of its own rather than a smaller one — built-in versus
  mounted-server is the `open_world_hint` distinction, i.e. whether the call
  could have reached outside the session, which is worth a standing visual.
- **A multi-line value is content, not an argument.** A string with a line break
  in it — a file's contents, a heredoc, a message — or a collection holding one
  leaves the signature entirely and renders below it, under its key, as a
  definition-list entry: highlighted when the call names a language or operates on
  a path with a known extension, verbatim with its own line breaks otherwise. A
  line break is the discriminator because an identifier never has one; in the
  recorded corpus it separates a write's contents and an edit's anchor from every
  path, pattern and flag with no length threshold at all.
- **Every other argument sits in the signature,** strings quoted (the escapes keep
  an embedded quote or comma unambiguous), other scalars bare, collections in the
  signature's own `key: value` register. The one exception is width-relative: a
  value longer than three whole lines at the current width is not something a
  reader scans as an argument, so it too leaves the signature and renders below.
- **A built-in's primary argument reads positionally.** A small table in
  `signature.ts` names it — `read`'s paths (an array spreads, one argument per
  item), `write`/`list`/`read_image`'s path, the edit family's `file_path`,
  `search`'s pattern, a process tool's handle — so `read("a.rs")`, not
  `read(paths: ["a.rs"])`. Positional arguments lead; the rest follow in the
  model's emission order. The table is keyed by wire name, so a mounted server's
  same-named tool keeps its labels: only a built-in's argument shapes are known.
- **Empty is empty.** A no-argument call is `check_inbox()` — no block, no `{}`.

**The layout is the pretty-printer's.** A call that fits on one line is one line; one
that does not breaks after `(` with an argument per row, and an argument whose value
is a collection that still does not fit breaks the same way inside it. A lone scalar
argument stays hugged by its parentheses — breaking around it adds two rows and the
value wraps as text either way. The signature is set in the monospace face, so a width
is a count of characters and fitting is arithmetic: `ui/columns.ts` measures the head's
column count (one shared `ResizeObserver`; a canvas `measureText` per font, forgotten
when a web font loads), and the layout is a pure function of the view and that integer,
memoized per call, recomputed only when the integer changes. Rows indent in `ch`, the
unit the layout counted in. A chip's added width (the path chip's ` ⤢`, a process
chip's resolved command) is budgeted as an upper bound, and a wide character counts
as one column; either can overrun a row, which then wraps as text rather than clipping.
Before the first measurement the signature renders flat.

Call bodies truncate at both widths
(`ui/truncate.ts`: 12 lines at desktop, 4 on the phone) — which is what stopped
a `write` from printing the whole file into the transcript.

Two tools stay special. **`bash` renders its command bare, after a prompt** — quoting a
shell line into `bash(command: "…")` reads worse than the line, and the tool's name adds
nothing a prompt does not say. The accent `›` hangs in the margin (`.shell-command`), so
the command's text lines up with the intent above it; the two clipping containers a call
can sit in (a capped item, the gate float's scrolling body) keep room for it. The command
is highlighted inline; one containing a newline is a block instead, carrying the same
prompt. How it runs, when that is not the default of blocking until it exits, is a tag
after it — `background`, or `background after 240s` — in the interface voice, so it is
never read as part of what was run; a shell `&` would have been, and the tool's own
description warns the model that `&` in a command means something else. A brief row
shows the prompt in place of the name too, and a multi-line command's first line. **The edit family emits no body**: the diff rendered below the head is
the representation there, so a body would show the change twice; the inspector's args
tab (`callArgs`, a separate computation) is where the whole input is asked for.

**The TUI renders the same form** (`src/tui/projection/signature.rs`, a port of
`signature.ts` and `signatureLayout.ts` with their test cases; docs/tui.md). Chips and
their width allowance are not ported yet, and a streaming call's `…` sits inside the
parentheses there rather than after them.

Tool **descriptions** stay out of the transcript. They are static per tool, so
in a run with 55 `ferritin.*` calls the same sentence would render 55 times,
while the call's own model-authored `intent` — already a sibling line — says
what *this* call was for. The inspector is where a description is paid for once.

The head's markup carries the wrapping rule. A flat signature is one inline flow,
`.call-sig`, whose tokens are ordinary inline spans and wrap as text; a broken one is a
block per row. `overflow-wrap: break-word` lives on `.call-sig`, so an overrun breaks
between tokens and only mid-token when one token is wider than the line. The head was a
flex row for as long as an affordance sat at the end of it, and flex items are
blockified by the CSS Display spec — so a signature built of sibling spans wrapped as
*whole boxes* onto their own lines. A chip rides the argument it refers to (below), so
there is nothing to place at the end of the row.

`ui/CallHead.tsx` is kept apart from `ToolBlocks.tsx` so it imports nothing but
React, the cordon, and the chip renderer it is handed: the markup is what the
wrapping test asserts on, and it renders with no highlighter and no media query.

## An argument can carry a chip

A **chip** upgrades one argument of a signature in place. `ui/chipTable.ts` is a
lookup keyed by `(tool, argument)`; `ui/chips.tsx` renders what it names, and
every other argument renders as the text the cordon computed:

```
kill_process(handle: ⟨p#4 · cargo run --release …⟩)
write(path: "src/api.rs" ⤢, contents: 505 lines)
```

The rule the table is held to: **a chip adds an affordance or a fact the
argument does not carry** — a link, a resolved handle. Never a re-wording. That
is what keeps the table from growing into a per-tool prose registry, whose first
consequence is a second rendering path for every tool this client has not heard
of, and whose second is `Searched src for "Listening"` in place of
`search(pattern: "Listening", path: "src")`. So `search`, `read`, and `list`
have no entries at all: their arguments already say everything.

Two renderers cover the table's seven entries.

- **A path argument is the file link** — `write`'s `path`, the edit family's
  `file_path`. Within-cwd only, as the viewer is categorically
  (`session/touchedFiles.ts`): an absolute or `..`-leading path yields no chip.
  A button, so the call's open-the-inspector click handler ignores it. This
  replaced a detached `open ⤢` in a trailing slot on the head, which named the
  file a second time to say which one it meant.
- **A `p#<n>` handle resolves to what it was** — `handle` on `kill_process`,
  `read_process`, `watch_process`. The handle is opaque in the transcript, and
  `GET /v1/processes/:id` answers for any handle ever minted, running or exited
  ([process-monitor.md](process-monitor.md)), so the chip states the spawning command
  beside it, truncated to a recognizable head. One renderer serves three tools.

`ui/processRecord.ts` is the fetch behind the handle chip: lazy (most calls
carry no chip at all), one flight per handle however many chips name it, and
memoized for the page. A `404` is memoized too — an id never minted into the
background stays never minted — while a transport failure is not, so a chip that
missed can resolve on a later render. The one staleness this accepts: a record
fetched while the process runs keeps its running tone after it exits; the fact
the chip carries is the command, which does not change.

**A handle with no record degrades to the plain argument**, and that is the
normal case rather than an edge — a foreground command that never backgrounded
has no lifecycle entry, and every session recorded before the records existed
answers `404` for every handle. The chip is an annotation, so having nothing to
annotate with costs the reader nothing: the signature reads exactly as it did.

## A record-only entry is marked

An assistant entry carrying `excluded` is record-only: `project` skips it
([transport.md](transport.md)). An empty completion (`"Empty"`) that carries
thinking leads with *Empty response — recorded, not in context, asked again*. Empty failed attempts produce no transcript
nodes; their current explanation belongs to transport chrome instead. Partial
thinking, prose and tool calls still render, led by an *Incomplete response —
recorded, not in context* marker without the raw failure body. Interrupted generations
retain a *Generation interrupted* marker, including when empty. When preceding
`Telemetry::Interrupt` records an origin, the marker names its client and affordance
(for example, *from the web stop button*). The fold uses the latest interrupt and
consumes it at the next assistant entry; user turns and new main request starts
clear it so an idle interrupt cannot name a later generation. Side-request telemetry
does not clear it. Logs without an origin keep the plain marker. Origins are
client-asserted, not authenticated identities.
`upstreamFailureWhy` reads either the string or object serialization of the mark.


## A call's input, while it arrives

While a call's input streams, the accumulated buffer is a *prefix* of a JSON
document. `render/partial.ts` (mirroring the TUI's `projection/partial.rs`,
[tui.md](tui.md)) splits it positionally: within an object only the last key can
be incomplete, because the bytes for the earlier ones arrived and closed. The
settled pairs go through the same `callView` a committed call renders with — so
the signature fills in (`write(path: "src/api.rs")…`) as bytes land and the
block keeps its shape when the entry commits — while the pair still arriving
renders
only as content: a multi-line string as a code block in the language of whatever
*settled* path names it, a single-line one as a dim `key: value…` line. A
half-arrived value never backs an identity claim (`write src/api.r` is
well-formed, plausible, and false), and before anything structural arrives the
head shows the bare name and the raw characters, which claim nothing — the empty
argument list is withheld there too, since `write()` would claim the call has no
arguments.

The split is read off the parser rather than guessed: a `partial-json` parse at
`Allow.COLLECTION` drops an incomplete trailing string along with its key, one
at `Allow.COLLECTION | Allow.STR` keeps it, and the key the second has that the
first lacks is exactly the value in flight — the same differencing the TUI reads
off jiter's two `PartialMode`s. Parity with the TUI's rule is pinned by
`render/partial.test.ts`, ported case-for-case, including the byte-at-a-time
sweep that no prefix ever settles a value the whole document contradicts.

## Prose is rendered as prose

`ui/Markdown.tsx` renders user turns, assistant text, thinking, and
summary text as markdown (`react-markdown` + `remark-gfm`). Tool
output does not come through it — the cordon gives it per-tool
representations — and that exclusion is the point of the split.

Thinking is markdown on the same footing as the rest. It carried a named
reversal trigger when it landed — a model whose thinking was line-structured,
bare adjacent lines markdown would merge into one paragraph, would have flipped
it back to preformatted. That trigger was retired 2026-08-25 as measured-inert:
across 653 blocks in 20 sessions (including `stealth/ox-alpha`, the most verbose
thinker seen here), 73.5% carry an explicit markdown construct — inline code 65%,
numbered lists 40%, bullets 28%, bold 22%, fenced code 16% — while the trigger's
predicate, a run of adjacent plain-prose lines, appears in 7.8% and is almost
always a colon lead-in above an `A.`/`B.` enumeration markdown does not read as a
list. Merging there joins a header to its first item; it destroys nothing. Nothing
requires thinking to be markdown, any more than prose is required to be; it simply
is. The TUI adopted the same treatment on 2026-08-26, inheriting this
measurement rather than re-deriving it ([tui.md](tui.md) for the dimmed
register it needed to do so).
A mark's note has no such caveat:
the projection re-injects it as a user-role text message, so it is prose on
the wire already. Distinct wire blocks within one prose body are joined with a
blank line, since a single newline would soft-wrap the join between them.

The renderer must produce a **React element tree, never an HTML string**. The
text is model-authored and the page holds a session credential, so a markup-
emitting renderer would mean `dangerouslySetInnerHTML` over untrusted content;
that constraint is what picks the library, not ergonomics. Two `react-markdown`
defaults carry it and neither should be switched off without reading
[Untrusted content](#untrusted-content): raw HTML
is escaped unless `rehype-raw` is added, and `defaultUrlTransform` passes only
`http`/`https`/`irc`/`ircs`/`mailto`/`xmpp`, so a `javascript:` or `data:` URL in
model output never reaches an `href`.

Prose styling is scoped under `.prose` and tightened against the browser
defaults, which are sized for documents: a transcript is a sequence of short
turns, and default heading and paragraph margins put more space *inside* one turn
than the entry separation puts *between* two.

## Syntax highlighting

`ui/highlight.ts` owns one Shiki highlighter; `ui/Code.tsx` renders through it.
Three decisions:

- **The fine-grained bundle**, over a preset. The app embeds into the binary at
  compile time, so the full grammar set is not affordable; the languages are
  exactly the ones `render/parse.ts`'s `langForPath` can name, loaded as dynamic
  imports so the grammars are their own chunks rather than main-bundle weight.
- **The JavaScript engine over precompiled grammars**, not the Oniguruma WASM
  one — no `.wasm` to ship, and no `wasm-unsafe-eval` needed in the CSP the
  bind serves. Of the two JavaScript engines it is the *raw* one:
  `createJavaScriptRegexEngine` carries a transpiler that converts each
  grammar's Oniguruma patterns to JavaScript `RegExp` at load time, and it sits
  in the main chunk. `@shikijs/langs-precompiled` ships grammars whose patterns
  are already JavaScript regex literals, so `createJavaScriptRawEngine` runs
  them with no transpiler: the main chunk loses 56 kB (19 kB gzip, back under
  Vite's 500 kB warning) and startup does no transpiling. The precompiled
  grammar chunks are larger than their source-pattern equivalents, but less so
  than the transpiler weighs — every chunk gzipped, the whole app is ~11 kB
  smaller. The literals carry the `v` (unicodeSets) flag, an ES2024 floor
  (Chrome 112, Safari 17, Firefox 116) the desktop browsers this targets
  clear.
- **Except bash, which runs the full grammar on the regex engine.** The precompiled
  bash grammar (shiki 4.4.3) anchors its argument rules on `\G` — "where the previous
  match ended" — which the raw engine cannot express, so the precompile drops the
  anchor and a command colors its first word and nothing after it. bash gets a second
  core of its own, `@shikijs/langs/bash` on `createJavaScriptRegexEngine`, loaded
  lazily: the transpiler lands in its own chunk (56 kB, 20 kB gzip), not the main one,
  which imports only the `@shikijs/engine-javascript/raw` entry. That core is warmed
  with one representative line before it is handed out: the regex engine compiles
  each pattern on first use, and a line that outlasts shiki's per-line time limit comes
  back with its tail as one uncolored token — memoized, so the first command rendered
  would have stayed that way.
- **Tokens, not HTML.** `codeToTokens` returns `{ content, color }[]` grouped by
  line, which the components render as spans. `codeToHtml` would return a markup
  string, and nothing on this page turns untrusted text into markup. The line
  grouping is also the shape the numbered-gutter view already wanted, so a
  triaged `read` is highlighted *and* keeps its per-line original line numbers.

Highlighting is best-effort: until the grammars load, and permanently if they
fail to, every caller renders the plain text it did before. The one rule carried
from the TUI is that a block is highlighted **only when its language is known** —
ferritin's renderer defaults to `rust`, which is right there and
wrong here, where the commonest untagged block is a tool result, so `ls` output
was coloured as Rust. `data-lang` stays on the element either way: it records
what the cordon inferred, independent of what got coloured.

Fenced blocks inside prose route through the same component, so prose code and
tool code share one language map. Inside a `<pre>`, highlighted lines are joined
by explicit newline text rather than one element each — an element per line gives
a blank line zero height, which silently eats every empty line in the source.

**Highlighting is memoized per block, and so is the cordon's parse.** The TUI's
"compute once per block, not once per frame" finding transfers, for a different
reason than repaints: a store update re-renders the whole transcript, so without
memoization every streamed token re-tokenized every code block in the session and
re-parsed every tool result. Measured by the page ceasing to respond for the
length of a turn. `useHighlight` therefore returns a *stable* function — an
unstable identity silently defeats every caller's memo.

The rule that actually bounds the work is the TUI's, stated the same way
([tui.md](tui.md)): **a committed entry is re-rendered only when the entry
changes.** `Entry` is memoized on its log entry, so a streaming turn re-renders
only the pre-commit tail. `toolOf` is memoized for that reason — a fresh closure
per render would break every entry's memo at once.

The one growing block — a call's in-flight value — cannot use that memo: its
text changes on every delta, and a whole-buffer re-tokenize per chunk is O(n²)
over a large file write. `StreamingTokenizer` (`ui/highlight.ts`) instead
resumes from Shiki's grammar state at the last complete line, so each delta
tokenizes only what arrived since; `StreamingCode` (`ui/Code.tsx`) renders
through it with the same markup as `Code`, so the block does not change
appearance at commit when it hands over. Its contract — tokenizing in chunks
equals one whole-buffer pass, at every prefix — is pinned by
`ui/highlight.test.ts`.

## Theming

`web/src/theme.css` holds every color as a custom property; components reference
tokens only. A theme is one override block: After Dark (dark, magenta accent) is
the default set on `:root`, and Lilac overrides under `:root[data-theme="lilac"]`.
The frames before the script runs show only the page background, which `:root`
paints onto the canvas; until `data-theme` is set, it follows `prefers-color-scheme`,
Lilac's under a light scheme, as the iOS launch images do ("Installability").
`index.html` links `theme.css` itself rather than leaving it to `main.tsx`'s import
of `index.css`: under the Vite dev server an imported stylesheet arrives only with
the module graph, and the frames before it paint unstyled white. `ui/theme.ts` resolves the
choice — explicit and persisted in localStorage, or the system scheme when none
is stored — and sets `data-theme` on `<html>`; `initTheme()` runs before first
render, and the select lives in the bottom panel's interface tab. After Dark's
legacy accent halo (`--accent-glow`) is transparent in Lilac. Glyphs use separate
`--neon-ink` and `--neon-text` tokens: a near-white core with a stacked accent
halo in After Dark, accent ink without a halo in Lilac.

Control states read role tokens: `--btn-bg` / `--btn-bg-hover` /
`--btn-border-hover` for secondary buttons, `--btn-primary-*` for primary
buttons, and `--focus-border` / `--focus-shadow` / `--focus-ring` for keyboard
focus. Resting primaries have no shadow. Hover uses `--control-halo`, a soft
accent halo in After Dark and a lilac-tinted shadow in Lilac. Keyboard focus
adds a separate outline to buttons; fields use their existing border and the
shared halo through `--field-focus-*`. The composer uses the same halo through
`--composer-focus-*`; its send button remains secondary because Enter sends
from the textarea. `button.quiet` keeps its borderless text treatment on hover
and has padding for its focus outline, with a negative margin returning it.
`button.linkish` uses brighter text and an underline on hover and press, without
button shadows; keyboard focus retains the shared visible outline and halo.
`ui/InteractionLight.css` calculates the boundary core and halo from a local
light level: resting controls are unlit, hover/focus uses 0.6, and pressed uses
1. The core whitens as intensity rises, while hue remains in the halo. Buttons
also animate a centered interior hue pool while pressed. The shared transition
is 180ms, disabled under reduced motion. Fold pills, session hover cards,
attention-blocked overview cards, current questions, and permission gates use
the same boundary calculations. Floated gates retain opaque page backing;
in-flow gates are transparent.

`ui/SelectionRows.css` shares panel, list, and row framing across model,
location, and archived-session choices, including even selected boundaries and
room around rounded row corners.

`--selection-border` gives selected picker and resume rows an even inset
boundary without a one-sided rounded-corner mark. Checked
checkboxes use `--badge-core` / `--badge-ink` / `--badge-halo`: a pale core,
dark text or checkmark, and accent light around it. Checkboxes retain native
keyboard interaction; forced-colors mode restores native appearance.
The user-turn gutter is a tapered two-pixel core with a soft accent spill,
drawn separately from its content box.

`--selection` is accent-tinted in After Dark and lilac in Lilac. Lilac's
`--accent` is heather, a muted red-violet distinct from the cooler lilac of
controls and focus, and `--warn` is apricot.
The session tabs' status dots use the same idea: a near-white core with the
status color in the glow, the glow scaled per status through a registered
`--dot-r` (a gate's is largest, your turn's smallest), and a running session's
glow pulsing while its core stays lit; an idle session is an unlit outline.
Lilac's dot is a white bead shading out to the hue, since a white core with
only a rim read as the idle outline on a light page.
Adding a theme
is authoring an override block plus a select option
([backlog/inbox/web-plain-themes.md](../backlog/inbox/web-plain-themes.md) is the pending
pair).

Strings and paths use misty blue (`--code-path`), while semantic diff additions
and removals retain their separate colors. Gallery palette wrappers redeclare
both that token and the relevant Shiki aliases, because aliases inherited from
`:root` already carry their resolved root colors.

Syntax colors derive from the same tokens. Shiki v4 dropped
`createCssVariablesTheme`, so `ui/shikiTheme.ts` hand-authors the same shape:
the one registered theme's "colors" are `var(--shiki-*)` strings, which pass
through tokenization into token colors, land on React's `style` prop, and
resolve in the browser against `--shiki-*` aliases of the semantic tokens. A
theme swap therefore recolors code with no re-render and no re-tokenize, and
the scheme subscription `useHighlight` once carried is gone. The passthrough is
pinned by `highlight.test.ts`.

Fonts are self-hosted: official OFL builds, unmodified, in `web/public/fonts/`
with their license texts beside them (the README there lists the files —
Newsreader variable roman + italic; IBM Plex Mono 400, 400 italic, 500; IBM
Plex Sans is deliberately absent and `--font-ui` falls back to `system-ui`).
Self-hosting is forced rather than chosen: the CSP is `default-src 'self'`, so
a Google Fonts link cannot load. The files ship whole because subsetting counts
as modification under the OFL's reserved-name clause. The serif is the voice of
prose — assistant text, thinking, user turns (the mock drew user turns mono;
reversed on use, 2026-08-19), the intent line — while everything tool-shaped
stays in the mono. The user turn's accent gutter carries no glow, dropped on
use the same day.

One asset trap, recorded because it will recur: Vite inlines small `src/`
assets as `data:` URIs, and the CSP has no `data:` source. Lilac's
paper-grain SVG therefore lives in `public/` (never inlined, served as-is),
not `src/assets/` — anything else CSS references by URL follows the same rule
or grows a hash in `default-src`.

## Transcript ergonomics

Two behaviours over the render-whole transcript. **Follow-tail**
(`ui/useFollowTail.ts`, `ui/followTail.ts`): the transcript scrolls the window.
Following is reader intent, not a continuously recomputed distance to the bottom.
A cold open follows the tail; upward window movement paired with wheel, touch,
keyboard, or scrollbar navigation holds the reading position. Content growth,
programmatic scroll events, and viewport changes do not independently cancel
following. Reaching the bottom by any means (within one CSS pixel for fractional
scroll positions) or an explicit latest/gate jump re-engages it. The transcript
pins after renders and observes document/body size and viewport changes for
asynchronous layout growth. Pinning skips redundant scroll commands when the
viewport already contacts the bottom and defers layout-driven scrolling during
an active touch/pointer press, resuming on an animation frame after release so
the target is not moved before click dispatch. Nested scroll containers and
editing controls retain their own input behavior. `JumpToLatest` subscribes to
the same intent rather
than measuring bottom contact separately. **The preamble is not
rendered**: the system prompt is audit data rather than conversation, and a new
session whose only entry it is reads as empty. **A title change** renders as
one quiet line, `Title updated to …`, and only when the text changed — the log
appends a title entry even when the peek kept the previous one. **A restart**
(`Resumed`) renders as the notice the model reads for it, in an ambient entry
(`ui/resumed.ts`, a port of `render_resumed`): the gap, lost background
processes, calls left pending, and entries skipped after an unclean end.
**A worktree event** (`Worktree`) renders as a one-line notice in an ambient
entry (`ui/worktree.ts`): an add or restore names the branch, with a failed
setup stated on the line itself; a removal says whether the branch went too;
a keep carries the reason and a failed removal the error. Git's setup output
sits collapsed behind a disclosure, with old progress redraws cleaned to their
last segment per line.

Ambient notifications (`ui/Ambient.tsx`) use the shared transcript output size
(`--output-font-size`, 0.8125rem) and monospace family, also applied to raw call
contents and tool-result code. They show at most three rendered lines by default;
a disclosure appears only when the preview overflows and expands the full text.
The source stays muted. Display-only cleanup (`ui/ambientText.ts`) removes ANSI
CSI formatting and OSC links/titles; stored text and agent delivery are unchanged.
The gallery has a dedicated Ambients section with short, process-output, ANSI
and wrapping specimens.

Ambient and config-entry content preserves its newlines with `pre-wrap` and
allows long tokens to break with `overflow-wrap: anywhere`. Single-line unfinished
tool arguments in `LiveCallBlock` reuse the `call-body-text` wrapping treatment;
multiline fragments use `StreamingCode`. Background commands, build-output URLs,
and paths can otherwise overflow a correctly sized content box and widen the page
even though its bounding rectangle fits the viewport.

**Density toggles** (`ui/viewSettings.ts`, persisted like the theme): two
switches hide thinking and ambient entries. Hiding is display-only — the
entries stay in the store and reappear on toggle. A spot verdict is drawn only on
the call waiting at the gate, as the reason alone; on every decided call — clear,
approved, or declined — the outcome is the call's result and the verdict is the
inspector's.

**The pending-turn echo.** A submitted turn the daemon accepted (a 202)
renders immediately as a dimmed user turn marked "Queued · the agent reads it at
its next step", with a Recall link beside it, after the live
tail, and flips to a committed turn when the matching `User` entry streams
back — the operator's delivery signal, which matters most across a reconnect,
when "did that land?" is otherwise unanswerable from the screen. The queue
lives on the session (`pendingTurns`), keyed by the submission id the submit
action mints and the daemon echoes onto the resulting `User` entry
(docs/control-api.md), so settling is an exact lookup: the fold drops the
pending turn whose id an arriving entry carries. Two orderings make delivery
arrive outside the fold, and both are handled: the entry's log frame can fold
*before* the 202 body resolves (the daemon commits and broadcasts
independently of writing the response), so the push skips itself when the
entry with its id already folded; and a re-seed (`seed`) settles pending
turns against the entries it absorbs, since those never pass through
`applyLog`. (The TUI mints and keys the same way.)

**An `ask` reads as a turn ending** (`session/tree.ts`, `ui/Transcript.tsx`). A call named `ask` (docs/process-monitor.md) with a usable `question` is not a call node: `buildTree` emits a leaf carrying the question and its place among the asks of one completion (`askOfCall`, the counterpart of `process_monitor::ask_of_call`), drawn as the assistant's prose with a mark, after the step's other calls. The mark (`ui/askMark.ts`) says where the question stands: `answering` (above the prose in a softly lit current-question frame, with ` · question n of m` when `of > 1`) for the ask the live `ask` topic's `call_id` names, which is the one the composer's send answers; `next · question n of m`, dim, for an unresolved ask while the topic names a sibling; `asked` (with the position) once its result has committed (`AskQuestion.resolved`, set by `buildTree`) or when there is no topic to say more, as in a replay. While the topic is non-null the composer's placeholder reads `your reply answers question n of m`, or `your reply answers the question` for a lone ask. Its result entry becomes a second leaf at its log position: with a `submission_id` it is the reply, drawn from `raw.content` under its sender as a `User` turn is; without one — an interrupt, a malformed call — a dim `not answered`. An ask with no usable question stays an ordinary call. A call whose arguments are still streaming is not drawn until it commits. A send while the `ask` topic is non-null is the reply, and names the question it answers (`answers`, the topic's `call_id`). A send refused because it named no question while one was pending — the question arrived after the text was typed — or named one no longer pending answers `409 not_delivered`: `submitTurn` folds the refusal's `ask` topic into the store, as `resolveGate` folds a moved gate, and the composer keeps the draft and says `not sent: a question came first` (or `that question is no longer waiting`). Its echo is keyed like any other, and because the fold settles on any entry carrying the id, the reply's `ToolResult` settles it; the echo reads "Sent · answering the question" and carries no Recall, since a reply is never queued. (The daemon's refusal to continue past a question, `409 not_continuable`, reaches the composer's error line like the other continue refusals.)

**An ask's recalled turns return to the composer once.** Turns queued before the question began are withdrawn by the daemon and published on the `ask` topic as `recalled`. Submission ids are recorded on the session (`minted`) **before** the POST, so a turn whose 202 has not resolved is still recognised as this client's; a restarted client has none and restores nothing. `applyFrame` on an `ask` frame with a `call_id` not yet seen (`askRestored`) drops the echoes of the minted ids among `recalled`, records them in `withdrawn` (so a late 202 does not queue an echo nothing will settle), and sets `askRecall`, which `SessionView` hands to the composer's `{text, attachments, nonce}` splice like a manual recall.

**The `asking` status word** is one of the listing's statuses: the session card reads "has a question" and shows the question as its activity (the `ask` topic is one of the card's topics), the attention is `your_turn`, and archiving an `asking` session is confirmed like archiving a running one.

Each echo carries a **recall** affordance: `DELETE /turns/:submission_id`
withdraws the pending-queue suffix beginning at that turn and answers with what
it actually popped (docs/control-api.md). Nothing was ever logged, so there is
nothing to retract. The action drops only the echoes the daemon reported
popping, and hands the texts to the composer, which prepends them to the draft
already there joined with a newline — via a `{text, attachments, nonce}` prop,
because recalling the same text twice is two
events rather than one value. The turns' attachments come back with them, still
stored, and are staged again. An empty answer is the lost race — the loop dequeued the turn
first — and the draft is left alone.

**A running tool's output streams into its call's result slot** — where the
triaged result will land — under a `running · N lines` header, in the pinned
box (`PinnedBox`, `ui/TranscriptItems.tsx`): a fixed height holding the newest
line, the top fading only while lines are scrolled out above. The triaged
result replaces it when it commits. Output whose call has not committed yet
still peeks as its own item below the tail. Tools that do not stream show their
result once the call returns, while it awaits triage (below).
Background-process output is read in the running-task strip's inspectors (below).

**Streaming flows into the committed state without a change of shape.** A call
whose arguments are arriving is the same item a committed call is — the intent
as its title (filling in as it streams, when it is the value in flight), the
signature, the bodies — and before anything structural arrives it is its bare
name. The live thinking tail is the pinned box; a long think that commits in the
recent tier stays in that box, stopped at its last line, with only its header
changing from "Thinking…" to "Thought · N words · show all".

**A summary** renders as a boundary: a dashed rule with
`⊙ consolidated #from–#upto · M entries` (`through #N` for a head summary; `compacted` for a
compaction), then
the replacing text as prose between the rules. It is drawn where the
agent's projection injects it — after the entry the range ends in,
a result counting as drawn by the entry that issued its call — not at the
summary entry's own log position, which for a background check can follow later work
(docs/consolidate.md). `buildTree` places summaries after its main pass, from the
node count recorded at each entry. The count is the turns, completions and
results in the range — client-computed from the window; the wire's token delta
is provider accounting the client does not hold. There is no expand affordance
behind it: render-whole means every entry the range replaced is on screen
above, so the rule's job is to say where the model's context stops carrying
the raw log, not to reveal anything. The `consolidate` call itself collapses to its
header once resolved — its input is the summary the rule renders in full
(the pair projects normally, so the log holds the summary twice), and its
result is a constant acknowledgement. A pending, gated, or failed
consolidate renders whole: the gate surface expects the call on screen, and
an error is information.

## Chrome and the content column

The page is full width; content sits in a centered `.col` (60rem). The chrome
— the top bar, the composer band, and the status bar —
bleeds to the viewport edges, and each bar's affordances anchor to the window
edges too: the wordmark and the app-settings link, the turn state and the
interrupt (full width for parity across the bars, 2026-08-20). Only content
keeps the column: the transcript and the composer input. Chrome is also invisible to text selection (`user-select: none`
across labels, bars, tabs, and controls) — only content selects: transcript
text, commands, results. Inside the column, entry content carries a right
inset balancing the label gutter's air, so text in a block does not run flush
against the column edge.

## The top bar

The browser document title follows the current session name, with ` · efference`
as the suffix; home and unresolved root views use `efference`. `ui/identity.ts`
shares the header's title/listing/id fallback, and `ui/documentTitle.ts` updates
the document in a layout effect and resets it when its owning view unmounts.
Session identity updates rename the tab without a reload; switching to a cold
session uses that session's listing title or id rather than the previous view's.

`ui/TopBar.tsx`: sticky interface chrome — the serif wordmark (under a host,
also the link back to the session list), the session's title
([session-title.md](session-title.md), the id until one is written), the
context meter, the session cost where the provider prices requests, and the
app-settings link. What the session is — models, directory — and how it runs
belong to the surfaces that show them whole; the bar carried them on one line
and could show none of them. The title is the one element that yields width,
and on the phone breakpoint it is dropped: the start of a title says less than
nothing. The **context meter** is the bar,
with `42.7k/1M` as a small caption under it so the numbers take no width of
their own. `ui/ContextBar.tsx` shares its fill with the switcher hover card:
occupancy is clamped to 0–100%, its halo grows and its core whitens with occupancy.
The rounded fill reaches cool white at full occupancy. Lilac has a continuous
colored rim and a wider hue cloud extending behind the caption; After Dark uses
the shared control halo. The hue moves
from the meter hue toward the accent between 50% and 85%, then toward the warn
hue between 85% and 100%. These are visual fill ranges, not consolidation
thresholds. Unknown usage draws no fill; an unknown window draws the count
without a bar.

**Session cost** draws beside the context meter only where the provider prices
requests; a direct Anthropic session has none, and its header is unchanged. It
takes the meter's caption line — `$0.51` in the same dim type, with the meter's
bar left out — because the provider's running total is a sum, not an occupancy.
`ui/cost.ts` writes the figure for the header and both session cards. A request
the provider's accounting endpoint could not price makes the total a floor, and
the presence of any such request prefixes the number with `~` rather than
naming a count: past the first, "inexact" is the whole of what the reader
needs. The header names the count itself in the element's title. Cost and
context are independent readings, each drawn where its own provider supplies it,
and not assumed present together.

`ui/PlanMeter.tsx` adds a subscription meter next to context occupancy only when
`usage.plan` reports a window; other providers leave the header unchanged.
The shared track has the existing glowing usage fill above a subdued elapsed-time
line. `ui/planMeter.ts` computes elapsed time from the reported duration and reset
instant, clamped to 0–100%; absent timing leaves the lower line unfilled. The client
clock refreshes every 30 seconds. Clicking cycles through reported short and long
windows, Codex first and then other named allowances such as Spark; absent allowances
never enter the cycle. The initial selection uses the backend's active allowance
when present. Captions show duration and used percentage, and name non-Codex
allowances. Hover/focus details use the prose typeface, named usage/time readings,
and GitHub relative-time elements for report and reset instants. A passed reset
retains the last reported usage and identifies it as awaiting an update rather than
assuming the allowance renewed. The gallery's Plan allowance section supplies
behind/equal/ahead, exhausted, unknown-timing and passed-reset specimens plus
Codex-only and Codex/Spark cycling examples.

The switcher card, context details and plan tooltip share the anchored surface in
`ui/HoverPopup.css`: surface background, light edge, ten-pixel corners, control halo,
shadow and padding. Meter details use the card's prose title and body sizes.
`ui/HoverPopup.tsx` opens meter details after 180ms of mouse hover or immediately on
focus; its padded positioning wrapper bridges the pointer's path into the panel.
Mouse departure closes after 120ms; focus acquired by a mouse click does not keep
that popup open. Keyboard focus keeps details open while focus remains inside.
Pointer events distinguish mouse from touch/pen on hybrid devices: a non-mouse
pointer release opens details, while mouse clicks do not open or pin them. Context
retains the summarize-history action and keyboard activation. Escape or an outside pointer
press dismisses meter details. Meter and session-card panels use
`ui/useKeepOnScreen.ts` to shift horizontally inside the viewport on opening and resize.
Plan clicks continue cycling allowances rather than acting as a popup toggle. Switcher
cards use the same hover delays and surface while retaining their session-navigation interaction.

**The session switcher** (`ui/SessionSwitcher.tsx`) is a second row inside the sticky
header, present under a host whenever a live session is listed, including when only the
selected session remains. It uses the same listing subscription the overview holds
(`api/sessions.ts`), so it adds one connection at rest. A tab is a status dot and a session
id; the dot is read from the listing alone through `cardModel`: a gated session is an
accent dot with a halo, a session whose turn ended or that is asking is the accent dot
without one — a finished turn waits on the operator until the session is archived — a
stalled one is the warn colour, a parked or blocked one the parked colour, a running one pulses faintly, and a session never started
is a hollow ring. Hovering or keyboard-focusing a tab opens a card
(`ui/SessionHoverCard.tsx`) with the overview card's status line, title, activity line,
context meter and cost; that opens the session's own state stream and log tail (`openSessionCard`)
only while the card shows. Clicking or tapping the selected tab toggles its card. Keyboard
focus can move into its actions; a click-opened card or an archive interaction keeps the card
open across pointer departure. Escape restores tab focus and closes the card; an outside
pointer press also dismisses it.

The card's navigation content and its actions are separate elements. The actions are
one box beneath the card: **Switch model…** above a rule (it rides the session's URL as
`?switch=model`, so the view behind the page stays mounted), and **Archive session**
below it. The archive action (`ui/ArchiveAction.tsx`) uses the overview's `archiveWarning`
rule: active work or background processes show a specific warning with “Archive anyway”
and “Cancel”; an idle session with nothing running archives directly. The action disables
while its request is pending, and the switcher reuses a pending request for that session
if its card is reopened. Failure stays in the card with a retryable action and does not
navigate. After a successful archive, the current session's route is replaced with home;
archiving another session, or entering another view while the request runs, leaves the
new view alone. The listing update removes the archived tab and closes its preview feed.

Recency seeds tab order from visit times in `localStorage` (`ui/switcherOrder.ts`), with
unvisited sessions following in the host's id order. Established slots persist across
session navigation. Tabs are measured against the row (`ui/useSwitcherFit.ts`); when they
overflow, the selected tab's measured width is reserved first, and other visible tabs
retain their relative order. Those that do not fit are listed in a `+N` menu whose button
carries an accent dot when a session in it is waiting on the operator; attention does not
reorder tabs. Entering a session from a tab is a client-side navigation, as from a card.

Below the top bar and session switcher, one wrapping transport notice shows the
latest failure, never a list of prior attempts. `ui/upstream.ts` combines the
level-triggered `upstream_failure` and `upstream_retry` topics without endpoint or
clock logic. `ui/upstreamMessage.ts` extracts `error.message` from provider JSON
(with string-error and top-level-message fallbacks), bounds the explanation, and
keeps unrecognized JSON and echoed SSE response data out of the notice. HTTP
status supplies a category; status-less failures describe interrupted generation.
Retry progress names the attempt and budget separately, with triage retries marked.
Counts remain diagnostic data but are not rendered as accumulated errors.

HTTP 2xx clears the failure. Retry progress may remain until finalization or
exhaustion, but never reintroduces the old retry detail after that recovery. A
later stream failure sets a new notice. The gallery's Transport status section
covers overload, usage limit, broken stream and recovery at desktop and phone
widths. Loop state stays in the gate cue and composer's status readout.
The page is `viewport-fit=cover`, which draws it under the status bar and beside
the notch, so the bar pads itself by the `safe-area-inset-*` values; sticky
elements below it dock at its measured height rather than a constant.

## The composer area

**One status slot.** `ui/ComposerStatus.tsx` renders the toolbar readout and the
collapsed phone footer. `ui/sessionStatus.ts` selects a startup failure (including
retry-refusal detail and recovery action), inspection error, startup progress, or
non-live connection, in that order. A local composer action error takes precedence
over those conditions. Otherwise the slot shows the current turn state. Errors
replace the turn readout; they do not stop the underlying turn or remove its
interrupt control. Clearing a condition reveals the current status rather than a
saved previous label. Startup retry remains available in either composer layout.
Errors use a muted warm surface, orange left accent, selectable wrapped detail,
and a separate recovery button where applicable. On phones this readout spans the
row above the controls, including when the input is collapsed. No session status
banner is rendered above the transcript.

**Drafts belong to the session, not the mounted composer.** `ui/drafts.ts`
keeps a `DraftStore` per session API base; `ui/Composer.tsx` subscribes to its
text and staged attachments. Client-side switching unmounts the view, not the
draft or its uploads. Upload requests, late removal requests, and image URLs
are bound to the originating base, even after the page changes sessions. This
store opens no session stream, so retaining a draft does not count as watching
that session or suppress its push notifications.

Each edit and attachment transition saves text and chip metadata to
origin-scoped localStorage under the session base. Completed attachments restore
from their stored server references; pasted text remains available for the
chip's inline action. File bytes and blob preview URLs are not persisted. A
reload restores an upload that was in progress as a failed chip with an
interruption reason, not a resumable upload. A failed chip states its reason in
its detail line, not only in its title, which a touch screen cannot hover to
read: the daemon's error message when it answered, or the browser's when the
request itself failed (Safari's `Load failed`). The daemon logs an upload's
failure at `warn` with its stage (no store, reading the body, storing), so a
failure that leaves no daemon line never reached the handler. Malformed saved data is ignored;
denied or full storage leaves the live draft usable without reload persistence.
Image blob previews are revoked on upload completion or chip removal, not on
view unmount.

A failed send leaves the draft alone. A successful send clears its submitted
text only if it has not changed while the request was pending, and removes only
the submitted ready chips; newer edits, newer attachments, and failed chips
remain. Sent files are not deleted. Empty drafts remove their localStorage entry.
The server has no draft store; these drafts remain browser-local.

The status bar (`ui/Composer.tsx`) is the dock's **outermost row, below the
composer at the window edge**, full-bleed — chrome frames content, as in a
native status bar (the row originally sat *above* the input, which put the
interactive content at the window edge; reversed on use, 2026-08-20). It pads
by `safe-area-inset-bottom`, so on a phone it clears the home indicator and the
rounded corners. At the left the turn state; at the right only the controls
that apply now: **interrupt** (`POST /v1/loop/interrupt`) while a turn runs,
and **resume** while the loop is held, because a hidden hold reads as a hang.
The other dials — pace and review level — are set over the control channel, not
from this client; hold is the composer band's own. The turn state reads the `loop_phase` topic: "your turn" in the serif when the
loop is idle, a dim `running…` while it moves (`retrying the upstream · send to
retry now` while an `upstream_retry` pause runs; `pacing · next step in 5s` during a
pace delay, counted down on the client's clock; while a request or tool is open, the `current_operations` topic's
leading operation with its elapsed time on the client's clock — `waiting on
<model> · 2m 10s` before the first byte, `<model> streaming` after it, `triage
· waiting 40s`, `spotter judging · 3s`, `bash: <command> · 45s`, `retrying upstream ·
attempt 2 (429) · 8s`; the turn's own request leads, then a running tool, then the
side requests, in `ui/operations.ts`; a `bash` call is labelled by its `command`, cut
at 48 characters with an ellipsis, and its `intent` is the hover title, while other
tools are labelled by the intent), `waiting for your approval` or `waiting for
your answer` in muted, upright UI text in the `gated` and `asking` phases (it outranks
the operation text, since the operator is what the loop waits on; a stall and a
park still come first), `held · resume or send to continue` for a held pace and
`stopped: the config file needs repair` for a config-repair park (the `blocked`
phase), and the daemon's own sentence plus `· send to retry` in the warn
color when the `stall` topic is set — a trajectory that stopped short, or a
triage held with the loop `blocked` ([transport.md](transport.md)),
which the phase alone cannot say; the upstream's words ride along as the hover
title. An empty send is the continue, which retries either wait. The choice is
`ui/turnState.ts`, a pure function of the phase, `stall`, `cold`, whether a retry
is waiting, and the operations view (the topic, the model, and the clock), so the component holds no branching of its own. The span between state and controls is
reserved for the key-hint strip (binds are deferred to interactive
suggestions; see "The design source" above). The input
carries no hint about mid-trajectory injection
([control-channel.md](control-channel.md)): the turn-state indicator beside
it already says whose turn it is, and riding along is what sending out of
turn does, so naming the mechanism cost prime UI space without adding
anything the operator acts on (a hint shipped here and was removed
2026-08-26).

The expanded input uses `ui/ComposerToolbar.tsx` for attach, the turn-state
readout, interrupt/resume, and send. At the 700px phone breakpoint the readout
occupies a full-width line above the buttons rather than wrapping between them.
The waiting readout has no dot or glow; its words state the pending action.

**Attachments.** A file reaches the composer three ways — pasted (a
clipboard image or file), dropped anywhere on the page (the window listens
for file drags, so the composer is the one drop target, and the input takes a
dashed accent border while one is over the page), or picked with the `attach`
button (the phone's way in, where there is no drag). Pasted text past a dozen
lines is staged as a `.txt` rather than typed, the same limit and rule as the
TUI ([inbox.md](inbox.md)); its chip offers `inline` to put the text back in
the draft instead. Each staged file uploads at once as raw bytes
(`POST /attachments`, [control-api.md](control-api.md)) —
`application/octet-stream` being the one other type the TCP bind's CSRF gate
admits — so its chip shows what the daemon made of it (`PNG · 1280×800 ·
240 KB`, `40 lines · 2 KB`) while it can still be removed. Chips sit above the
input; an image's chip shows a local object URL while uploading and the stored
bytes, resolved through `useApiImage`, once ready — the local picture stays up until
those bytes arrive.
Removing one deletes the upload; one removed mid-upload is deleted when the
upload lands. Send waits for uploads to finish, carries every ready chip's
name, and removes the submitted ready chips only once the turn is accepted,
so a refused send keeps them. A draft of attachments alone is a turn, not the continue. The
paste and description rules are `ui/staging.ts`; draft and upload ownership are
`ui/drafts.ts`; the chips, staged and sent, are
`ui/Attachments.tsx`. A sent turn — and its pending echo — shows its
attachments under its prose, an image as a thumbnail of the bytes at
`GET /attachments/:name`, resolved through `useApiImage`, that opens in the
lightbox.

**Sending an empty composer is the continue gesture.** With no draft, send
(and Enter, where Enter sends) issues `POST /v1/loop/continue`
([control-api.md](control-api.md)) — control handed back with no input, no
turn submitted, nothing appended — instead of the earlier silent no-op. Where the
`verbs` topic says the route refuses, its `message` renders in the composer's error
slot without a request; a `409 not_continuable` from a route that moved on since is
the same normal answer, so the daemon's `message` renders there on its own rather
than as a thrown-error string.

**A cold session says what the next action reads** (`ui/coldLine.ts`). While the
`cold` topic is set, a notice above the composer says `cache expired — a send
reads ~120k tokens uncached` (the topic's `context_tokens`) and offers the
alternative — *summarize instead*. The send goes through on the first press: the host
takes a turn or continue on a cold session like any other
([control-api.md](control-api.md)), and the notice is information. A pending gate or
`ask` carries its own line, so the composer's notice yields to it. The gate's controls
and the composer of a pending `ask` carry one line in the warn colour: `cache expired —
approving or redirecting reads ~120k tokens uncached` at a gate (the decision that
lets the turn go on, a redirect included, because its message is read against the same
cache), `cache expired — your answer reads ~120k tokens uncached` at an `ask`
(`kilotokens` of the topic's `context_tokens`; `the context uncached` when it is
unknown). A decision or answer is sent as it is whether or not the line showed; the
host records it as the person action that releases a parked loop
([control-api.md](control-api.md)).

A loop parked mid-turn at its cold check (the `cold_parked` phase) reads ahead of the
operations, so `turnState` says `paused: cache expired · send to continue (reads 120k
tokens uncached)` in the warn colour; the composer shows the cold notice with the same
*summarize instead* button. After a compaction the park stays and `cold` is null; the
readout says `paused: summarized · send to continue`, with no notice.

**Side-model previews** (`ui/SideStreams.tsx`, `session/summaryPreviews.ts`) are
transient, not additional transcript entries. A live compaction or detached
consolidation account uses the same coverage-to-drawn-node placement as the eventual
Summary, so newer work remains below a background check's frozen span. Account text
renders as Markdown between summary boundaries; thinking uses the shared bounded,
bottom-pinned thinking box (`ui/Thinking.tsx`) and the thinking visibility setting.
Compaction's cut comes from its operation or recorded request after the operation ends.
The composer retains the compaction operation notice while the preview occupies the
transcript. Triage thinking appears in a bounded panel above the composer; arriving
verdict text has its own collapsed disclosure rather than being mixed into thinking.
Durable curation removes the temporary panel, leaving the result's kept-line readout.

**Summarizing is compaction** (`POST /loop/compact`, [control-api.md](control-api.md);
`ui/compact.ts`, `ui/CompactNotice.tsx`, `ui/CompactControl.tsx`). Two entry points share
`compactOffer`: the cold notice's button and the standing action in the context-meter
popup (`ui/ContextPopup.tsx`). Opening the popup then choosing **Summarize history**
starts the operation without a further confirmation; the request supplies consent
to spend a live cache when the `verbs` topic's `compact` entry says the route needs
it (`warm: true`). Both entry points are the route's published answer: blocked, with
the route's own reason shown, where the entry refuses — a gate or question pending, a
loop doing something, nothing to compact — and ready otherwise, a loop parked at its
cold check or held at a triage included. The popup prevents duplicate clicks while its
request is pending. Progress is the compaction in `current_operations`: the turn-state
readout and popup say `summarizing the history…`; a failure is the
`compaction_failure` topic and stays visible for retry; and the summary lands in the
transcript. The hover
panel closes on outside interaction or Escape and stays inside the viewport.

The input is inset into the dock rather than floating on it: the composer
band shares the dock's surface color, and the box is near-square (2px) in
the chrome's flat language. The field auto-grows with the draft
(`field-sizing: content`, 3–10 lines), replacing the manual resize grip; a
`1ch` right padding gives the block caret its cell at the wrap column, where
zero padding clipped it to a sliver. A `contenteditable` composer (live
markdown styling) is an anticipated replacement for the textarea; the
attachment handling above listens on the textarea's paste and the window's
drag events, neither of which depends on the field being a textarea.

The controls strip is gone; its last resident, the usage line, lives in the
top bar. A non-`live` connection replaces the turn readout in the composer footer.

## App settings

`ui/AppSettings.tsx` exposes browser-wide preferences from the home and session
headers. The fullscreen settings dialog reuses `AppHeader` and the page/theme
tokens; its title uses the prose font. The `settings` query parameter addresses
the view without adding a host route. Opening, closing, and browser Back leave
the underlying session or home view mounted, including composer and launch
state; query-only navigation does not reset window scroll. The modal dialog
provides focus containment, background inertness, and Escape dismissal.

`ui/Interface.tsx` supplies the existing theme and thinking/ambient
visibility preferences and browser push registration, reached from the
app-settings link in the header. Preference storage and push
permission/subscription behavior are unchanged; notification prompts run only on
explicit interaction.

## Process output seeding

`session/processSeed.ts` reads `GET /v1/processes/:id/output` once when the processes topic first names a process, folding retained bytes beneath deltas already received. Warm buffers are skipped to avoid duplicating the retained snapshot. Process monitor retention loss is stated above the output. Task shortcuts and the call inspector provide process access; there is no separate session-details panel.

The gate cue's bottom offset is measured from the dock's height with a
`ResizeObserver` rather than guessed in CSS — the composer changes that
height.

## The inspector

`ui/Inspector.tsx`: a call-scoped right drawer, opened by clicking anywhere
on a call (hover tints the connector; clicks on controls inside, and clicks
that end a text selection, don't count) — "raw is one tap away" promoted
from the per-result `<details>` to a standing surface. Clicking the
inspected call again closes the drawer (the call is a huge central target
where the `✕` is a distant corner one — Fitts); a different call switches
without closing, and the `✕` remains. The
selection is a call id held in `App`, which builds the transcript tree once
and shares it with both consumers; the node is re-found from the tree each
render, so a result or verdict landing after the drawer opened shows up in
it. At desktop widths of at least 1100px the inspector occupies a sibling column; narrower screens use an overlay sheet. Its left edge is a drag-to-resize grip — width clamped
at both ends and persisted like the theme. It is opaque: a frosted variant
(translucent surface over `backdrop-filter` blur) was tried and dropped —
re-blurring a backdrop that is also moving stuttered against the slide on
modest hardware, and deferring the blur until after the slide left a
noticeable fade-in for little gain (2026-08-20).

The drawer slides in and out (`transition` on `transform` only — composited,
never layout; the resting state is the identity transform). Entry is
CSS-only via `@starting-style`; exit needs App-side state because React
unmounts before CSS can animate: `shown` holds the last call id through the
slide-out, unmounting on `transitionend` with a timeout fallback
(`transitionend` does not fire in a hidden tab), and reopening mid-slide
cancels the pending unmount. Switching calls while open does not re-slide —
the element never unmounts. The unmount handler keys on the *transform's*
`transitionend` specifically, so a future transition on another property
cannot unmount the drawer early.

Tabs — `result` / `args` / `raw` / `spot`, all views over data the client
already holds, rendered through the cordon's existing renderers rather than
a second implementation.

The **args tab renders the call's whole input**, unconditionally — a separate
computation (`render/parse.ts`'s `callArgs`) from the header's
`callView`'s bodies. Serving both surfaces from one value is what made the tab
say "no body — the header says it all" on every edit, which was false: the
header shows `file_path · operation`, while `anchor` and `content` — the
substance, a median 752 bytes of input across the corpus — went unshown.
`callView`'s bodies stay empty for the diff family because the diff below the
header *is* the representation there; the args tab asks a different question,
which is what was asked for. Multi-line values render as highlighted code
blocks — by the call's own `language` when it names one, else the extension
of the path it operates on — because a pretty-printed JSON scalar puts source
on one line with literal `\n`s and no highlighting, the least readable form
of the one thing worth reading (the same failure the TUI diagnosed once in
`write`). Long blocks fold to a readable head with a line count and a
`show all`. Scalars sort ahead of blobs whatever order the input serialized
in: they are the call's identity, and a blob above them pushes "what did this
touch" off the top of the tab. `intent` continues to render above the body
rather than inside it — it is already the call's leading line.

The tab (`ui/ArgsTab.tsx`) also **destructures the call against the tool's
declared schema**, fetched from `GET /tools/:name`
([control-api.md](control-api.md)) by `ui/toolDefinition.ts` — lazily, one name
at a time, memoized for the page: the catalog is snapshotted before the loop
starts and is session-constant, so a definition once read is the one every call
in the tab was made against. A `404` is cached with it (an unknown name stays
unknown, and *every* name is unknown in a recorded session opened with no daemon
behind it); only a transport failure is left uncached. The join itself is pure
and lives apart from both the fetch and the rendering (`render/argSchema.ts`).

The tab **leads with the tool** — dotted name, optional `title`, and the
description. That is the one place a tool description is paid for: on demand,
for a call someone is already asking about, which is why the transcript never
shows it (a static sentence repeated across 55 `ferritin.*` calls is noise, and
the call's own `intent` occupies that line with something specific). Each sent
argument is glossed with its schema description and marked `required`. The two
mismatches nothing else surfaces are the value beyond presentation: a key the
schema does not declare is flagged in place (the `crate` vs `crate_name` failure
shape), and the declared properties the call omitted are listed below, dimmer,
with a required-but-missing one flagged — the shape of a whole class of failed
calls. `intent` is excluded from *both* sides of the analysis: a toolbox tool's
served schema always declares it, so leaving it in would report "declared, not
passed" on nearly every call and gloss a line the tab already renders above the
list.

Server prose is third-party: a description is rendered as **plain text, never
markdown** (it never reaches `Markdown.tsx`), and a definition from a mounted
server carries a dim `described by <server>` line covering the tool's own prose
and its arguments' — the untrusted-server line the built-ins' annotations
already draw, surviving to the pixel. A built-in (`server: null`) needs no
attribution. With no definition at all the tab renders exactly what it rendered
before any of this — the sent arguments, unglossed, with no error surface and no
retry spinner.

Historical operation and explicitly current-file context are separate views in the same sidebar. The `result` and `raw` tabs also carry **the command's own output**, under
whatever the committed result showed, for the two cases where a `ToolResult` is
not the output ([process-monitor.md](process-monitor.md)):

- **A call still running** has no committed result at all. Its live tail is
  already in the store as `streaming.toolOutput[callId]` — the transcript's
  inline peek renders it — and the drawer reads the same buffer instead of
  saying "no result yet".
- **A backgrounded call**'s committed result is the *handoff* result naming
  `p#<n>`, not the output; the output lives in the process monitor's retained
  capture. The drawer reaches it through the call↔process mapping: the daemon
  carries the spawning `call_id` on each process, and `store.ts` folds every
  pair it has seen into `session.processCalls` — kept after the process leaves
  the level-triggered `processes` topic, since the capture outlives the exit and
  the id is how it is read. While the process is live the store's own
  `processOutput` buffer is used; once it has exited the drawer reads
  `GET /processes/:id/output` directly. A client that connected after the exit
  never saw the row and has no mapping — the registry is ephemeral either way.

A capture that hit the retention cap says so — "output incomplete; N bytes
dropped at the retention cap", from the route's `lost` — rather than presenting a
truncated capture as the whole output.

Below the tabs,
**what the fork returned**: the raw line count, the surfaced count computed
from the keep, the keep itself (`All` / `None` / `Ranges(a..b, …)`), the
wire state, and the triager's note — the audit view of
[triage.md](triage.md)'s output. Stepping between calls from
the drawer waits on the deferred key binds.

With the inspector holding the whole body, **read and write results
truncate in the transcript**: the first 12 lines, then a `⋯ N more lines`
indicator (never for a tail shorter than the indicator itself). There is no
per-result `inspect →` link: the whole call is the inspection affordance, and
its hover state is the discovery surface. This is the first revision to
render-whole, and it is
safe for the reason the position existed: a gate decides a *call*, never a
result, so no pending decision rests on a truncated body — and the full
body is one click away. Other tools' results still render whole.

## Captured file operations

`ui/inspector/FileOperation.tsx` provides file-specific inspection instead of generic tabs. Reads show captured per-file bodies; `fileCapture.ts` maps retained raw-result positions into each body, strips explicit ranged-read number/tab prefixes, and preserves original file-line coordinates. Captures may be partial, range-limited, or byte-truncated; they are not assumed to be whole historical files. Canonical capture paths confirm removal of range selectors for current-file navigation. Submitted write content comes from call arguments, with append labeled as appended bytes rather than a whole file. Failed/pending writes are qualified. Edit operations show the full recorded diff, not reconstructed whole-file history. Raw arguments/results, permission verdicts and curation remain under Details.

`ui/RangeContent.tsx` is a display-only expansion component backed by captured original text and inclusive retained ranges. The whole gap row is a button revealing up to ten lines per available edge; middle gaps use one ⇅ control to reveal both ends together. Intersecting ranges merge. Retained lines have a gutter marker, and revealed context has the same text contrast. Highlighting runs over the full captured source before selecting visible rows, preserving grammar across gaps. Expansion never changes the agent's curated output. Changing the captured source resets expansion; unrelated renders preserve it. The gallery has plain and highlighted specimens. `ui/ResultRanges.tsx` adapts transcript results to the same component, preserving raw-result coordinates through read framing removal and diff classification. Pending output uses the same numbered gutter as decided output, inside its bounded pinned box. Decided selections up to 20 lines show whole; longer selections start with ten visible retained lines. Expanding a hidden selection does not alter its retained status.

## The file viewer

`ui/FileView.tsx` reads the current file from `/files` ([web-bind.md](web-bind.md)) in the same sidebar position as operation inspection. At desktop widths of at least 1100px it is a sibling column; narrower screens use the inspection sheet. The transcript remains mounted. `ui/inspector/useDrawerWidth.ts` shares resize width between operation and file views. An Operation return restores the mounted historical inspector, including expanded ranges. Closing ends inspection; browser Back steps from current-file context to the operation before leaving inspection. A direct tree view has no operation to return to.

Call path chips still open current-file context directly. File-operation bodies offer View current file or Locate in current file; other tool inspectors retain a current-file footer. `session/touchedFiles.ts` supplies paths and anchors. Current content is explicitly labeled current, not reconstructed historical content. **Within-cwd only, categorically** — an absolute or
`..`-leading path yields no affordance and is never followed; the call's own
log diff is its surface. That is a scope position (no arbitrary reads from
elsewhere on the system), not a resolution gap. The `session_identity` topic
([control-api.md](control-api.md)) now names the cwd, so it is the mapping
available if the scope ever widens.

**Highlight sources do not mix.** With a selected edit anchor, an exact location highlights its surviving added lines; approximate/unlocated matches show a qualification and no confidently located-line tint. Refreshes keep resolving that selected anchor. Without an anchor, `session/lineDiff.ts` compares fetched content with the last content shown for the same session/path, marking observed changes and deletion points. This is changes since viewing, not the selected operation's historical diff. Source mode tints numbered lines; rendered Markdown uses block-level highlights.

**Freshness is two sources through one mechanism.** A committed, non-errored
edit in the log (the `lastEdit` derivation) supplies a new target while following; the daemon's change stream covers edits from outside — a
`fetch` of the same `/files` URL with `Accept: text/event-stream` (`fileStream`,
`api/files.ts`; the content negotiation and the watcher behind it are
[web-bind.md](web-bind.md) "Watching for changes"), held while the tab is
visible and closed while hidden. The reader parses the frames with `sseData`,
reports each connect and each poke, and reconnects with the capped backoff
`openStream` uses after a refusal, a failed request, or a stream that ends.
Every poke, and every (re)open of the stream — which is what covers changes
missed while hidden or disconnected — answers with the same conditional GET, so
the stream carries no content and replaced only the old 2 s poll's timer when it
landed. A refusal drops the held validator, because a file restored with its old
mtime would otherwise revalidate 304 against the refusal screen.

**The edit itself is the second source of "where to look."** The
since-you-last-looked diff is empty on a file's *first* open — which is every
file follow switches to — so on its own it landed the reader at the top of the
file with nothing highlighted. `session/editAnchor.ts` answers from the edit's
own result instead: an edit tool's `===DIFF===` payload
([tools.md](tools.md)), parsed by the cordon's `diffRows`, yields the lines it
inserted and the context they sat between. That anchor is located **by
content**, not by line number, because the payload carries no hunk headers —
there are no post-edit line numbers in it to trust, in the live case either.
Locating is verbatim first, then with each line trimmed (the structured editors
reformat around an edit, and a re-indented block is the same block):

- a unique match is **exact** — jump and highlight, which is what the live case
  always is, since the edit that just landed is the newest change;
- an ambiguous match, or one found only through the surrounding context — the
  historical case, where later edits moved or removed those lines — is
  **approximate**: the viewer jumps and says *position approximate — the file
  has changed since this edit*, rather than highlighting confidently in the
  wrong place;
- nothing found at all says so and does not jump.

`App` carries an open request (path, the call that asked, and a key that
changes per request) so a second call's path chip on the file already shown
re-runs with *its* anchor; `anchorForCall` reads that call's result.
Anchor-driven loads are unconditional — a `304` would carry no bytes to locate
against.

**Following the agent** is implemented and held off: with it on, an edit
landing elsewhere would switch the view to that file and land on the edit
(edits and writes, never a bare `read`). It has no control, because whether it
is a viewer setting or a session-level mode is undecided, so the view moves
only when the reader asks — a path chip, a tree click, a link. An edit landing
on the open path re-reads it either way; that is live reload, not following.

**Refusals render legibly against the path** — "not a text file",
"too large to view", "no such file under the working directory" — never a
blank pane: the endpoint refuses rather than degrades, so every body that
arrives is whole, and there is no truncation state, windowed fetch, or
virtualization here. Markdown files default to rendered with a
`source`/`rendered` toggle; a leading `---` metadata block renders as a field
list above the prose (`ui/frontmatter.ts`), its lines blanked rather than
removed so the changed-line stamp still maps by source line. Other files are highlighted source with
absolute line numbers. `api/files.ts` is the fetch (refusals as values, not
thrown errors, because the viewer renders them).

## Navigating the working directory

Without this the viewer reaches only files a call already named, which is the
set the reader already knows about. The motivating case is the opposite one:
the agent is working, a file you have not seen probably holds the answer, and
you want to look before steering it wrong — including from away from the
machine.

The **tree** (`session/useFileTree.ts` + `ui/FileTree.tsx`) is lazy: one flat
listing per expanded directory, no recursive walk, and each expanded directory
holds a directory-scope change stream (`dirStream`, the same reader;
[web-bind.md](web-bind.md)) so what is on screen stays current. As with the
viewer, the answer to stream *open* is the same re-list as the answer to a poke,
which is what covers a change landing during stream setup. The root is expanded
at mount.

The **filter** above the tree (`session/pathMatch.ts`) matches paths from one
recursive listing, fetched the first time anything is typed; while it holds
text, the matching paths replace the tree, with arrow keys moving a cursor and
Enter opening it. Escape clears it, and on an empty filter closes the sidebar.
Each space-separated word must occur in the path as typed, contiguously, with
smart case; each listed path marks the text that matched. A word scores its best
occurrence — inside the basename, at the start of a segment, at the start of the
basename — and shorter paths break ties, so the file beats the vendored copy of
it. Searching file *contents* is not built:
path matching is a listing the client already has, where content search is a
server-side scan with its own cost, ranking, and result-rendering questions.

The **annotations are derived client-side** from the transcript the store
already holds (`fileActivity`, `session/touchedFiles.ts`); the endpoint
contributes nothing and knows nothing about the session. Files the session has
read are the agent's working set and read bold; files it has changed take the
accent. This is where a read registers at all, since reads deliberately do not
move the viewer.

Entry is the session header's files button (`ui/TopBar.tsx`), which opens the
sidebar on the tree with no file chosen and closes it again, or the `/tree`
route — the one client route besides `/`, which opens the viewer the same way.
The tree and the file share the sidebar one at a time, so choosing a file leaves
the tree. The sidebar's head is the path (`ui/FilePath.tsx`): the working
directory's name, then each directory down to the open file, where every segment
but the file returns to the tree opened at that directory. On the tree the head
is the name alone. A relative link in
rendered markdown resolves against the open file's directory and opens in the
viewer (`ui/fileLinks.ts`), a directory link in the tree; every other link opens
in a new tab, since following it in place would unload the session view. Both routes are named in `SPA_ROUTES` (`src/daemon/tcp/gate.rs`), which is
also what the service worker's offline fallback mirrors.

The listing's ignore set is gitignore-derived and lives on the server
([web-bind.md](web-bind.md)); a file git cannot see is not discoverable here,
while remaining readable and followable by name.

## Installability

`public/manifest.webmanifest`, `public/sw.js`, and the icons: the app installs
to a home screen and runs in `standalone` display. The icon is a schwa (`ə`),
the mark the efference site's favicon uses, on the After Dark page background,
as an SVG (`public/icon.svg`, the source) and the PNG sizes derived from it. The
maskable icon keeps the mark inside the 80% safe zone. `theme-color` starts at
After Dark's page background and follows the resolved theme (`ui/theme.ts`),
since the standalone window's chrome takes it.

The launch screen is separate from the icon. iOS shows a home-screen app's launch
screen from an `apple-touch-startup-image` whose size, orientation and pixel ratio
match the device exactly; without one the screen is white until the page paints,
and iOS does not read the manifest's `background_color`. `scripts/startup-images.py`
draws one plain, centred wordmark, Newsreader pinned to `wght` 400 and `opsz` 72, at
every size in its device table in both orientations, in the After Dark and Lilac
page colours read from `web/src/theme.css`. Its output is 76 PNGs under
`web/public/splash/` and the link block between the startup-images markers in
`web/index.html`; re-run it after changing the wordmark or the table. iOS takes the
image when the app is added to the home screen, so a changed image shows only after
the app is re-added. The launch scheme follows `prefers-color-scheme`, because a
static image cannot read the stored theme choice. The wordmark is laid out through
raqm, with the font's kerning and ligatures. The gate serves `/splash` anonymously
(`ASSET_PREFIXES` in `src/daemon/tcp/gate.rs`), like the rest of the shell; the worker
does not precache them, since iOS reads them only when the app is added.

The service worker does three things: make the app installable, let the shell
paint from cache on a cold start or a lost network, and receive push
([web-push.md](web-push.md)) — handlers at the bottom of the file that touch
none of the caching rules. **Nothing
session-shaped is cached** — `/v1`, `/files` and `/auth` always reach the
daemon, matched with any session prefix removed so the rule covers both
bindings; the shell routes are matched the same way, so a session-prefixed
navigation falls back to the same cached shell. A transcript or a file's bytes served from cache would be a stale claim
about what the agent did, and saying what the agent did is what this page is
for. What is cached is the build's own hashed assets (cache-first, because the
hash is the version) and the shell routes. The few public files the build does
not hash (the manifest, every icon, `paper.svg`) are precached by name, and
`src/daemon/tcp/gate.rs` names the same files as anonymous; changing their bytes means
bumping `CACHE` in `sw.js`, or installed copies keep the old ones. Registration
(`src/pwa.ts`) is `/sw.js` in production and `/sw.js?dev=1` under Vite. In dev
mode the worker installs no cache, answers no `fetch`, and deletes every
`efference-shell-*` cache on activation, so a cached asset never reaches HMR and
a production worker left on the same origin is replaced (its script URL differs)
and leaves nothing behind; the push handlers are the only live part, which is
what lets a dev-proxy host hold a push subscription. The daemon routes by path,
so the query reaches the same file in the embedded build and under the proxy.
Push — the worker's third job, and the "receive
notifications" opt-in in the Interface tab — is [web-push.md](web-push.md).

## The gate surface

The pending approval gate (`web/src/ui/Gate.tsx`) renders from the `gate` state
topic, in the transcript flow rather than a modal, and **inside the gated call's own
entry**. A gated call has no result entry, so the call is the transcript's last item;
`Transcript` marks it (`gated`: rendered whole whatever its recency level, with the
shared lit boundary and halo, with no tinted fill) and mounts an empty slot under the
call, into which the surface portals its explanation and controls. Reading order is
call → explanation → decision, all in one box: the decision is visibly *about* the call
rather than a panel following it. `GateReason` supplies the same explanation in flow
and in the floating panel. For an ordinary gate, it shows the `Spot` verdict's text
without a stance label. For an escalated gate, it shows “escalated by <id>” from the
last hop's `source` (or “you” for the operator's command line), followed by that hop's
note. A nonblank escalation note replaces the visible spotter reason; without one,
the spotter reason remains, labeled “spotter:”. Earlier hops do not render here. The
spotter verdict remains available in the inspector, including after a decision.
The controls are **Allow** and **Decline…**, which opens the redirect
(a growing field: decline and say what to do instead) and a quieter "stop this turn
entirely" (abort). The surface once rendered the call itself, through `JSON.stringify` —
which put a `write`'s file body on screen as one escaped line on the one surface where a
human commits; placing the decision in the call's own entry is what removes that
duplication. The `gate.reason` fallback survives for a gate held with no `Spot` entry
behind it — a window the stream's coherence rule closes
([control-api.md](control-api.md)) — as does an in-column fallback for the moment
before the gated call is in the tree; both are robustness rather than paths that
normally render.

Because the affordance sits in the flow rather than pinned, it can be scrolled
away from. While an `IntersectionObserver` reports the in-flow controls out of
view — or the file viewer is open, which occludes the surface without moving
it out of the observer's geometric view (the `covered` prop from `App`; the
float renders above the viewer, its jump-to-call link dropped because the
tail it would land on is behind the viewer) — a **floating gate panel** pins
above the dock — and it is the one place
the call renders a second time (via `CallBlock` and `GateReason`, under the
transcript's own classes: intent, full command, explanation), because the entries
the in-flow surface leans on are exactly what is off-screen. It is drawn as the
gated entry lifted: the entry's fill and border, at the entry's measured left edge
and width, plus a shadow. The panel shares
the in-flow surface's form state — one set of drafts, rendered in both places —
so a decision composed in one survives into the other. It collapses, without
resolving the gate, to a compact cue naming the call (it may be covering text
the operator is reading); the cue expands it back, and it re-expands for each
new call. Two properties are deliberate: the floating surface **never scrolls
the page on its own** — a pending decision does not get to move the page out
from under someone reading — and its `jump to call ↓` is a jump to the document
tail (the gated call has no result, so the gate *is* the tail), not a smooth
`scrollIntoView`. Its jump explicitly resumes follow intent; subsequent renders
and layout growth pin the tail. A navigation to an earlier transcript position
must instead disengage follow intent rather than relying on a programmatic
scroll event to identify a reader departure.

The decisions ride `PUT /v1/approvals/:call_id` (`Decision` in `api/types.ts`):
approve, abort, and redirect with steering text. The route also takes a
`config_edit` grant, which the web client does not offer: a grant guessed from
the call's input was a command prefix whatever the call gated on. The host now
offers one on the gate itself (`gate.grant`, [control-api.md](control-api.md)),
which the client does not yet render.
A `409 gate_moved` is handled in
`session/actions.ts` by folding the response's corrective `detail` into the
store as a synthetic state frame — no second round-trip; `resolveGate` returns
`"moved"` rather than `"applied"` so the surface can say the decision did not
land.

When the pending call changes under the surface, the inputs always reset — a
decision composed against one call must not land on another — but whether a
notice appears depends on who caused the change. After the surface's own
resolution, the next call gating is the expected course (a batch with several
gated calls does it on every approval, and the daemon's diffing topic
publisher may coalesce away the cleared state between), so the reset is
silent. An unprompted change means another session decided the call; the
notice says so, mentions the cleared draft only when one existed, and renders
informational rather than in the error class.

## Untrusted content

Everything the transcript displays is untrusted by construction — model-authored
text, tool output, file contents, MCP server responses — on a page that holds a
session credential for a control channel that runs commands. One invariant and
one backstop.

**Nothing turns untrusted content into markup.** JSX text nodes and React-escaped
attributes are the only sinks; there is no `dangerouslySetInnerHTML` and no
`innerHTML` assignment in `web/src/`, and an eslint rule
(`no-restricted-syntax` in `web/eslint.config.js`) keeps it that way, so reaching
for one has to be an argument rather than a habit. This is why the markdown
renderer must produce elements and the highlighter must produce tokens: both were
chosen against this constraint rather than for ergonomics.

The one payload-derived attribute is `data-lang`, from `langForPath`'s closed set
or a `/language-(\w+)/` capture. Links come from `react-markdown`'s
`defaultUrlTransform`, which passes only `http`/`https`/`irc`/`ircs`/`mailto`/
`xmpp` — so a `javascript:` or `data:` URL in model output never reaches an
`href`.

**A `Content-Security-Policy` on every response any listener produces**
(`SecurityHeaders` in `src/daemon/tcp/headers.rs`), alongside `cross-origin-opener-policy:
same-origin`, `x-content-type-options:
nosniff`, `referrer-policy: no-referrer`, and `x-frame-options: DENY` (redundant
with `frame-ancestors` for anything current, and one header for browsers that
predate CSP3). The transport-side headers the layer also sets — `cache-control` on
`/v1` and `/files`, HSTS on a TLS bind — are described with the bind
([web-bind.md](web-bind.md)). It sits outermost in the handler
stack so its `before_send` runs last, and the headers land on the SPA index, a
hashed asset, a `/v1` rejection and a 404 alike — a handler that answered without
them would be the hole. The layer is on the Unix socket's stack too, so `nosniff`
does not depend on which listener a response came from. The stack is built once by `handler()`
(`src/daemon/handler.rs`) and used by both the listener and the tests, because a
hand-copied test stack is what lets the two drift.

The embedded build loads one external module script and one external stylesheet,
both same-origin and hashed, with the grammar chunks imported dynamically from the
same origin — so `default-src 'self'` needs no hash, nonce, or `unsafe-` escape.
Highlighting needs no `style-src` accommodation: token colours are set through
React's `style` prop, which is CSSOM, and CSP governs inline `<style>` elements and
`style=""` attributes parsed from markup rather than CSSOM mutations. Images add
one source to `'self'`: the client draws the bytes it fetches from the API
(`useApiImage`) and the local preview of a file being attached through object
URLs, which `'self'` does not match, so `img-src` names `blob:` beside it. `data:`
is not named, so an inline image stays blocked. Under `--features dev-proxy` the
page is Vite's — an inline module preamble and an HMR socket — so the policy
relaxes, gated on the same `cfg` as the proxy so a release build cannot serve it;
it carries the same `img-src`. A test asserts the shipped policy contains no
`unsafe-`, another pins `img-src 'self' blob:` and the absence of `data:`, and a
third that it names no source but `'self'`, `'none'`, and `blob:`.

CSRF is a different threat, and the credential's form answers it: the bearer rides
an `Authorization` header that only this page's script sets, so a request another
site makes the browser send carries nothing that authenticates
([web-bind.md](web-bind.md)). Script running in this origin can read the bearer, as
it could read a CSRF token out of the DOM, so the bearer's boundary is this policy
and the no-markup rule above: no script but the build's runs here. The policy also
names no other origin for script to connect to or load from; a navigation is outside
what it governs.

## A session that is starting

Creation and resume answer once the host has reserved the session, so the view opens
while it is prepared ([control-api.md](control-api.md), "A starting session"). While the
`starting` topic is set, the composer status takes the connection readout's place and
says what preparation is doing (`ui/startingLine.ts`): running the env hook, loading the
config and mounting MCP servers, or waiting for a named MCP server to be authorized in a
browser on the host. The composer works: a turn sent now is queued and is the first the
session runs. When the topic clears the session has started, and the view reconnects the
ordinary way — the bounded tail, then the stream from its cursor — rather than reading a
resumed session's whole log through the stream it opened at zero. The overview lists a
starting session as working, "is starting".

A failure takes the composer status slot, says why, and offers the retry. The turns the failure hands back reach
the composer the way a recall's do, once per failure. For a resume the retry is the same
`PATCH` again, then a reconnect on the same view, composer intact. For a creation it is
**Back to the form**: the form reopens on the draft that made the session
(`ui/launchRetry.ts`, which keeps each creation's draft by the id it made) with the
failure's reason beside **Start session**, and the session it next creates receives the
failed view's composer text. An archived card whose last resume did not start says so,
with the reason (`resume_failure`).

## Following a hand-off

A session that hands off (docs/tools.md) names its successor in `session_identity`, and
`SessionView` replaces the page's location with the successor's (`ui/identity.ts`
`successorHref`) — `replace`, so Back does not return to the archived predecessor. The
successor's transcript opens on the predecessor's brief, whose `User` entry carries the
reserved `handoff[<id>]` source (`api/types.ts` `handoffPredecessor`): drawn as a harness
note ("handed off from `<id>` — its brief:") rather than as the operator's turn.

## Serving and auth

The gate enumerates its anonymous surface rather than deriving it from guarded
prefixes ([web-bind.md](web-bind.md)): the app shell, the hashed assets, and the launch
images under `/splash` serve without a credential, as do `POST /pair` and
`POST /pair/claim`, and everything else, `/v1` and `/files` included, takes a device
token. The sign-in view (`web/src/ui/Login.tsx`) carries the sign-in
([web-bind.md](web-bind.md)). A sign-in link's `#claim=` fragment is read and scrubbed
at bundle load (`ui/bootClaim.ts`), and on a later hashchange, so `efference dev open`
landing in an open tab works; the view claims it at `POST /pair/claim` with a label
naming the browser (`ui/deviceLabel.ts`). On a bare visit, or a link that names
nothing, it asks `POST /pair` under that label and shows the short code with
`efference devices approve`, the command that approves it, and a countdown, then
claims every second until the host answers with this browser's device token, which it
keeps (`api/bearer.ts`); the app takes it from there (*A refused bearer is decided in
`apiFetch`*, under *Layering*). A code that expires says so and offers a new one. The
SPA index answers the client's
named routes and nothing else (`SPA_ROUTES`, `src/daemon/tcp/gate.rs`) — a broader index
fallback would answer scanner probes `200`. On a host those routes are also answered
under a session prefix, which is the one place the anonymous shell sits among guarded
paths ([web-bind.md](web-bind.md)). The exact shell routes remain available when
that session is absent: the host resolver lets them reach the frontend, whose
session API read observes `404` and navigates home. Missing-session API and file
routes still answer `404` after their credential checks. One more GET is anonymous:
on a *session* bind,
`GET /sessions`, which is no route of its and answers `404` — that `404` is how
`resolveEntry` reads "not a host" (`web/src/session/entry.ts`), so the probe has to
reach the router rather than stop at a `401`.

## Development

- `cargo run --features dev-proxy -- --host --bind …` — the daemon spawns Vite and
  proxies to it; HMR works through the gate, because that build widens the
  anonymous surface to Vite's module URLs (`VITE_PREFIX` and `DEV_PROXY_DIRS`,
  `src/daemon/tcp/gate.rs`) — the shell has to load before anything can authenticate.
- Without the feature, the build embeds: the `frontend!` macro runs `vite build` at
  compile time, so building the crate needs `pnpm` on PATH and `web/node_modules`
  installed (`pnpm install` once).
- `pnpm check` in `web/` — typecheck, lint, format check, vitest.
- `pnpm dev` standalone also works: `web/vite.config.ts` proxies `/v1`, `/auth`, and
  `/pair` to a daemon on `:7878`.

### Finding what widens a phone layout — `web/src/overflowReport.ts`

With `?overflow` on the URL (dev builds), a fixed panel lists the viewport's
width, the document's scroll width, and the outermost elements whose right edge
passes the viewport. It also lists innermost visible text spills: elements whose
content is wider than their box, excluding content confined by a hidden or
scrolling ancestor. Both are readable off a simulator screenshot, where no
inspector is attached. A column wider than the screen shows on iOS as enlarged text (text
autosizing inflates an overflowing block), not only as a horizontal scroll, so
"the text is huge" on a phone is a width bug first.

### The design gallery — `web/src/gallery/`

With `?gallery` on the URL (dev builds), the page renders the client's widgets
on fixture data instead of the app. A section index links to composed scenes,
buttons, neon glyphs, session
tabs and their hover card, composers, code/results, images, session cards, archived
cards and rows, the switch page, launch settings, the model picker, and resume lists. Specimen labels stack above their
controls on narrow screens; state comparisons wrap rather than widening the page.
`SessionArchiveSpecimens.tsx` shows idle archiving, active-session confirmation, pending
feedback and retryable failures through local callbacks, without sending session requests.
`SwitchModelSpecimens.tsx` renders the switch page from a fixture identity and from a
dormant row's recorded selections, and `ArchivedSpecimens.tsx` the archived card and the
recent rows with their menus; the page's picker and its read of a dormant row are left
out, since both need a host.
`SessionChromeSpecimens.tsx` composes the production `TopBar`, fixture session tabs,
and `SessionStatusFixture.tsx` with selectable environment, MCP mounting,
authorization, startup failure, retry-refusal, reconnecting, and inspection-error
states. The fixture uses the production `ComposerToolbar`, `ComposerStatus`, and
`sessionStatus` selector. Retry locally enters its disabled state; tabs and overflow
do not navigate. The same header fixture replaces the composed scenes' placeholder
header. Gallery headers stay within their specimen rather than sticking to the
window.

`Scenes.tsx` renders a shortened worktree UI exchange in finished, input-arriving,
working, answer-needed, and approval-needed states. Input-arriving shows an
unfinished single-line scalar argument to exercise transient wrapping. It uses
`TranscriptContent`, the
production transcript body without window follow-tail, built from fixture log
entries through the normal tree projection. Tool output includes the triaged
kept-line rendering and its note; working shows live output, asking shows the
question's answering mark, and approval controls occupy the call's gate slot.
Answer-needed contains two questions, with local controls to show either as
current; the second state includes the submitted reply to the first question.
The current question has a lit frame and an answering label above its prose,
while upcoming and answered questions are unframed. This distinguishes the
answer target from the user turn's left-edge highlight.
Approval-needed shares production `GateControls`, including Allow, the
Decline redirect field, and stop-turn action; its reason comes from a fixture
Spot entry. The gallery does not mount the floating gate lifecycle.
Scene submit, attach, interrupt, inspection, and approval interactions stay local;
they do not issue session actions. The local raw-output disclosure is not the
app's inspector.

The composer fixtures share `ComposerToolbar` and `turnState` with the app.
Code/result specimens use the production
call and result components, including shell argument colors, edit-result framing
for colored diffs, and errors. The input, resume list, and picker shells retain
production class names but duplicate markup; those shells can drift when their
components change. Context specimens share `ContextMeter` and compare 10%, 50%,
75%, 90%, 100%, unknown usage, and unknown window. Checkbox specimens compare
unchecked and checked states; each form pins a hovered control or row alongside
its resting and selected controls. Fixture lists live in `fixtures.ts`.

`ImageSpecimens` renders the production zoom target over fixtures of three aspect
ratios — wide, square, and tall — so the lightbox's fit and gestures can be worked
on without driving a session to an image result. The fixtures are files under
`public/gallery/`, referenced by path rather than imported: an import of an SVG
that small is inlined as a `data:` URI, and the client's CSP (`img-src 'self'
blob:`; `src/daemon/tcp/headers.rs`) does not admit `data:` images. The gallery mounts
`LightboxHost` so the specimens actually open the viewer.

Controls also appear pinned in hover and focus states: `pseudoStates.ts` copies
every rule naming `:hover`, `:focus-visible`, `:focus-within`, `:focus`, or
`:active` with the pseudo-class rewritten to an `is-*` class, and re-derives the
copies when a stylesheet changes, so under HMR the pinned states follow the edit.

### Scrubbing the stream — `web/src/session/record.ts`

A dev build interposes a recorder between the stream reader and the fold.
`connect` takes an optional `FrameTap`; the recorder retains the latest 20,000
frames in arrival order and decides when the store consumes them. `session/frameTape.ts`
uses a ring buffer; each evicted frame folds into the tape's base snapshot, so
seeking within the retained window reconstructs the exact state. Eviction does
not stop live folding. A held store snapshot remains independent of the tape;
if its position is evicted, it stays displayed and is marked expired until an
explicit step, seek, or resume refolds within the retained window. Cursor values
are relative to the retained window, and the recorder reports the evicted count.
The connection is otherwise unchanged — same catch-up read, same `GET /events`, same frames — so
what a held position shows is a state the client genuinely passed through.
Nothing is synthesized, reordered, or stood in for.

It exists because a pre-commit state is real but momentary. A tool call's input
arrives over a few hundred milliseconds and is then superseded by the committed
entry, and a screenshot taken through a browser extension shares the event loop
with the render it is trying to observe. Holding the fold removes the race
entirely, where slowing the stream only widens it.

- `hold` stops folding; frames keep arriving and keep landing on the tape.
- `step(n)` folds `n` more, forward one at a time so the render re-runs per
  frame the way it does live — a position reached in one bulk fold can hide a
  stale-memo bug that only appears when each delta drove its own render.
- `seek(n)` puts the store at exactly `n` frames, refolding from the tape's base
  when it goes backwards. `connection` is carried across that refold rather than
  rewound: no frame carries it, so restoring it would report `connecting…` over
  a session that is streaming.
- `resume` folds everything held back and tracks the live end again.

Driven by `window.__scrub` from devtools, and by a `efference:scrub` DOM
CustomEvent — an extension's scripts run in an isolated world that shares the
DOM and nothing else, so a global the page set is invisible to them. Installing
the recorder prints the interface to the console, and each command logs the mode
and position it landed on (`[scrub] scrub 137/412`).

While investigating stream recovery, dev session views also install a temporary,
non-interactive overlay (`ui/streamDebug.ts`). A one-second timer reads diagnostics
without updating the session store or rerendering React: connection attempts,
raw byte reads including heartbeats, delivered frame counts and sequence,
folded store sequence, last reported view render, and recorder mode/window.
The overlay remains visible for screenshots; session unmount removes it and its
timer. Production builds omit the recorder and overlay. Raw activity observation
in `api/stream.ts` does not change heartbeat parsing or reconnect policy.

A recording is saved to IndexedDB (`session/tapeStore.ts`) once it holds streamed
output — a reload that only reconnects records a state snapshot, and saving those
would push the recordings worth returning to out of the store. Each recording is
its own record keyed by session and start time; a session keeps its newest five,
so a reload starts a new recording rather than overwriting the last. IndexedDB
rather than localStorage because a tape's base is the whole seeded session, tool
results included. `tapes()` lists them; `restore()` replays the newest earlier one
(or a named key) held at its end, and stops recording live frames until reload so
a replay never mixes with the stream. Serving a recorded session through the
daemon's own code remains the only way the TUI gets the same
([../backlog/inbox/session-replay.md](../backlog/inbox/session-replay.md)).

The whole module sits behind `import.meta.env.DEV` in `useSession.ts`, which a
production build folds to `false`; the branch and the module go with it.

### Command inspection and running-task shortcuts

`ui/TaskStrip.tsx` places running-task shortcuts above the composer. Foreground processes enter after one second; explicitly backgrounded processes enter immediately. Selecting a shortcut opens the originating call without scrolling the transcript. `ui/inspector/useHistoricalCall.ts` resolves calls outside loaded history into an inspector-only window, merging subsequent live entries without changing transcript history.

At desktop widths of at least 1100px, `ui/inspector/Layout.css` places the inspector beside the session column; narrower screens retain the overlay sheet. Bash inspection uses `ui/inspector/CommandView.tsx`: one heading, full Bash-highlighted command, independent wrapping output viewport, and live process actions. Output follows while running until the reader scrolls upward; bottom contact or Follow output resumes. Finished processes have no following controls. Structured telemetry supplies background handles after cold navigation; process records supply observed exit status and final duration. Missing end records remain unknown rather than implying success.

Raw results, permission verdicts, telemetry, and curation details remain under native disclosures (`ui/Disclosure.tsx`). Other tools retain the generic inspector tabs. `ui/inspector/handoff.ts` recognizes explicit and automatic handoff instructions against recorded handles: the transcript presents a compact Backgrounded label, while process output excludes those instructions. The underlying result remains unchanged.
