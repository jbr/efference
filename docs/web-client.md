# The web client

A React single-page app served by the daemon's TCP bind ([web-bind.md](web-bind.md)):
the second human client after the TUI, and the mobile client. Sources live in
`web/` and are built and embedded into the serving binary at compile time
(`trillium-frontend` in `src/daemon/tcp.rs`); there is no separate static host. It is
a client of the same `/v1` surface as everything else ([control-api.md](control-api.md))
— it holds one stream connection and derives everything from its frames, exactly as
the reference TUI does.

## Positions (from the plan that gated it)

- **Parity of information, not parity of interface.** The transcript is whole and
  faithful: every log entry renders, because a gate resolved over a snippet is not an
  informed choice. What it does not copy is the TUI's furniture — control strip, focus
  model, block menu — which is form-factor idiom, not shared invariant.
- **A tool result's primary view is what the model saw.** The distillate is the
  output; raw is the audit view, one tap away. Rendering that view client-side
  runs through the cordon (below) — decided, deletable debt, not the "shared
  home" a promoted UI finding normally gets. The long-term shared home is
  structured tool output (audience-tagged content blocks, or per-tool result
  schemas), which deletes the cordon whole; the reserved `render` field on log
  frames remains the daemon-side alternative if structure is slow to arrive.
- **Content before styling.** This gated the first slice; with the information
  surface substantially built, styling now arrives from the design pass
  ([backlog/web/README.md](../backlog/web/README.md) records the mock source and
  how to read it), starting with the theme layer below.

## Layering — the churn gradient

The tree is ordered by expected rate of change, so display iteration never touches
the wire:

| layer | job | React? |
| --- | --- | --- |
| `web/src/api/` | mirrored `/v1` types (`types.ts`), the control base (`base.ts`), the fetch wrapper + error envelope (`http.ts`), the SSE reader (`stream.ts`), the host's own routes — the listing stream (`sessions.ts`) and the new-session surface (`sessionForm.ts`) | no |
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
tool-execution phase later, reading as "it resolved itself". So an `Assistant`
commit puts the fold in a `draining` state and block deltas are dropped until
the next log entry ends it; an assistant response never directly follows another,
so the drain cannot swallow a real one. Tool and process output are unaffected —
they are keyed by call and process, not scoped to a turn.

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

The catch-up read and stream opener retain structured HTTP failures. A `401`
parks the connector for login; a `404` marks the session `missing` and stops
retries. `SessionView` replaces a missing host session's page with the root,
without a toast, so Back does not return to the absent session. A known handoff
successor takes precedence over going home. Network failures and other HTTP
errors stay on the reconnect path rather than triggering navigation.

### Coalescing the fold

A long block arrives as thousands of `output` deltas, and the pre-commit tail
that displays it re-parses the whole block as markdown when it renders. That
cost is linear in block length while the delta rate is roughly constant, so the
work over one block is quadratic: measured at ~8 ms per render of a 10 kB block
in the suite's environment, past a delta's arrival interval well before a block
of that size finishes. Once the folding thread is behind, the deltas already
buffered in the fetch body keep it saturated after the producer has gone quiet —
so the page did not recover when the stream ended, only when it was reloaded
([../backlog/web/streaming-hot-loop.md](../backlog/web/streaming-hot-loop.md)).

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
(`backlog/host-process.md`). `/auth/…` and
`/sessions` are deliberately *not* joined: they are the listener's own surfaces,
at its root under either binding — one credential covers every session behind a
host.

The id set the derivation accepts mirrors the gate's (`plain_id`, `src/daemon/tcp.rs`):
a path whose second segment is not a plain segment is not a prefix, and addresses
the root instead.

**What the root shows is a probe, not a configuration.** With an empty base the
binding is genuinely ambiguous — the shell is byte-identical — so `session/entry.ts`
asks: the host answers `GET /sessions` with the listing, a session daemon serves no
such route and answers `404`, and a host with no credential yet answers `401`
(which is the login view, after which the probe re-runs). Anything else — offline, a
daemon that went away — reads as a session, which is what a root has always been;
the session view's own connection banner reports the network state, where a second
failure screen would only restate it.

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
directory, one card each, in a stable order — workspaces by path, cards in listing
order — so a card stays where the operator last saw it and urgency is carried by the
card rather than by moving it. A workspace heading sets the path's last component as
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
`blocked` (gated, stalled or parked at the cold check: nothing moves until someone acts), `your_turn` (idle after
a turn — waiting on a reply or an end, not "less important"), `working`, and `new`
(never made a request and has no title). The top line is one sentence with the id as
its subject — `amber-lattice needs approval`, `hollow-birch is waiting for you`,
`tidal-cipher has stalled`, `amber-lattice is paused: its cache expired` (status
`parked`; the activity line says `continuing reads 120k tokens uncached`, and after a
fold `is paused: summarized, waiting for you to continue`) — and working and new say nothing, working being the
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
state topics (`gate`, `stall`, `running`, `usage`, `session_identity`,
`upstream_retry`). Glancing at the overview reports nothing: attention is what the
page's own report says ([web-push.md](web-push.md)), not what it has open.
`foldRecent` reads the latest intent and reply off the log; a user entry retires the
reply it answers, and a record-only assistant entry says nothing. That is N
connections and the whole log for a card that reads two fields, accepted while
whether the intent line earns its place is learned (`backlog/host-overview-stream.md`).

**Ending is the one thing here that is not a navigation**, so it sits behind the card's
menu rather than beside the title. `DELETE /sessions/:id` reaps the session's
background processes and removes its scratch, and the host answers only once that
teardown has run — so the card leaves on the next frame of the listing stream, which is
already out by the time the call returns. The confirm rule is `ui/sessionRow.ts`, the
same rule the TUI's switcher applies ([tui.md](tui.md)): a session that is doing
something, or that holds processes an end would reap, confirms in the menu naming
which; an idle session with nothing running ends on the first click. The top bar's
"New session" and each workspace's `+` open the new-session form below.

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
drawer, the file tree and the composer's local state are per session, and a pop between
sessions is a remount.

**Warm sessions.** A session's `Store` comes from `session/registry.ts`, which retains the
stores of the last `WARM_SESSIONS` sessions viewed. Leaving a session closes its connection
and keeps its snapshot; returning renders that snapshot on the first frame, and the connection
resumes from the cursor — a tail read merged into what the store holds (`reseed`, so the
scrolled-through window is not replaced) and the stream from `lastSeq + 1`. A retained store
has no connection of its own: an open stream would count as someone watching and suppress that
session's push notifications (`web-push.md`), so a session that is not on screen costs nothing.
The banner for a non-live connection is delayed by CSS for the first 0.8 s, so a resume that
lands quickly does not shift the transcript. The first visit to a session in a page's lifetime
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
are unmounted with it.

`ui/App.tsx` is only this choice — list, login, or session. The session view is
`ui/SessionView.tsx`, which is the component the rest of this document means when it
says App holds a piece of state (the inspector selection, the open file request, the
overlay count).

`ui/AppHeader.tsx` supplies the shared header frame and wordmark navigation.
`TopBar.tsx` supplies session identity, usage and drawer controls; the overview
and launch pages supply their own actions. The wordmark links to the session
list, with a callback available for draft-preserving navigation within the launch
flow. Modified clicks keep native link behavior. A page without a list can pass
a null home URL to render a non-link wordmark. Caller refs attach to the actual
header so overview docking still measures its rendered height.

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

What narrow changes:

- **The label gutter stacks.** The entry grid drops to one column — label above
  content, left-aligned — because a 6rem gutter plus insets is ~40% of a phone's
  width. The user turn's accent border is on the content, so it survives.
- **Every tool's result truncates to a glance** — 4 lines, the `⋯ N more lines`
  marker, the note — because at 32ch a long result is minutes of scrolling. The
  tap-to-inspect sheet holds the body; this is the drill-down posture arriving
  empirically. Desktop keeps read/write-only truncation at 12. A call's elided
  argument (a write's contents) truncates on the width's budget at *both*
  widths — the inspector's args tab holds the whole — and the marker appends
  `· tap to inspect` on narrow, because touch has no hover to discover the
  affordance with.
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
- **The drawer is a bottom sheet** (see "The drawer"), dismissed by dragging
  its header down.

**Overlays are history-backed** (`ui/overlayHistory.ts`, both form factors):
each open overlay — inspector, file viewer, drawer, lightbox — owns one history entry, so the
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
for one head. A distiller note and markdown prose break inside long tokens
(`overflow-wrap: break-word`), and the note additionally sets `min-width: 0`:
as a flex item its default `auto` floor is the longest identifier it contains,
so a note full of them — which is what a note about code is — grew its box past
the row and had every line-end cut at the screen edge. Both halves are needed
there; `min-width` alone lets the box shrink while the unbreakable token still
paints past it.

**The jump-to-latest chip** (`ui/JumpToLatest.tsx`, both form factors): a fixed
chip above the dock, shown only while the viewport is away from the tail,
tapping it an instant jump — the gate float's constraint (never smooth-scroll
toward the tail; `useFollowTail` re-pins mid-animation) travels with the shared
`jumpToTail` in `ui/float.ts`, extracted from Gate.tsx when this second consumer
arrived along with `useDockOffset`. Hidden while a gate is pending (the float
owns that slot and carries its own jump) and while the file viewer covers the
transcript.

Known nit: wire-vocabulary glyphs quoted in an *inline-code span* (a model
writing `` `⟢` `` in its own prose) render as tofu on iOS — IBM Plex Mono lacks
U+27E2 and the iOS mono fallbacks don't fill it; the serif covers it, as does
the renderer's own `✧` note chrome (backlog/icebox.md holds the fix candidates).

## The cordon — `web/src/render/`

A deliberate, temporary duplicate of daemon and TUI rendering logic, kept in
one directory so it can be deleted whole when structured tool output lands.
Two residents:

- `distill.ts` — a literal port of `src/project/render.rs` (the verdict
  renderer: keep-ranges resolution, `@@` hunks, the `⟢` note, the truncation
  breadcrumb, triage-failure and elision texts), tests mirroring
  `src/project/test/distilled.rs` case for case.
- `parse.ts` — a literal port of the TUI's per-tool parsing and dispatch
  (`src/tui/projection/tools.rs` + the per-wire-state match in
  `src/tui/projection.rs`): the `===DIFF===` classification for the edit
  tools, `read`'s sentinel stripping and extension→language map, the call
  signature below with the `intent` leading the entry, and the distilled
  composition — the parse runs over the whole raw, the verdict's kept line
  numbers filter the rows it produced, so a triaged view keeps its tool's
  rendering. The `kept N of M lines` header takes its numbers from the entry
  view's `distillate` field when the daemon sent one
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
[redaction-fork.md](redaction-fork.md) "Capture, then verdict") renders its raw
output at once, in the pinned bounded box a running command's tail uses, under a
`result (awaiting triage)` head. The `ResultVerdict` entry that decides it is
folded in `buildTree` onto the slot of the result it names — the slot's entry
becomes a copy of the `ToolResult` carrying the verdict's wire and line counts,
at the result's own seq — so the slot re-renders as the triaged result and the
verdict is never an item of its own. The first non-Pending verdict for a Pending
result decides it; a verdict naming a result that was not Pending is ignored. The
fold runs over the entry list, so the initial window and live frames take the
same path; results logged directly with their final wire render as before.

A result's images render below its body as buttons over `<img>` elements pointed
at `GET /v1/log/{seq}/images/{idx}` ([control-api.md](control-api.md)): the entry
view carries only `{media_type, width, height}` — the attributes that hold the
aspect box before the bytes land — and the browser supplies the fetch (ambient
session cookie), the `ETag` revalidation, and lazy loading. Capped at 420px
display width; tapping one opens it full screen in the lightbox. The inspector's
result tab carries the same images, so a picture-returning call is not a drawer
of everything but the picture.

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
label-free user turn, and may reverse back after use). Non-voice entries keep
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
  when something is actually hidden), the distiller's note lifted out below the
  cap; brief is one row — the intent as its title, the tool and what it acted
  on (its string arguments) beneath, the note under that.
- **Thinking**: trimmed is a three-line preview under the same fade; brief is a
  "Thought · N words" pill. Thinking of 120 words or fewer is never folded —
  the median block is about that size, and folding it costs a click and saves
  nothing (backlog/web/thinking-autocollapse.md).
- **Always whole, at any distance**: user turns and assistant prose; a call
  still running or held at a gate; a failed call; a result withheld by a triage
  failure; a call the spotter gated or watched. Errors, gates, and refusals
  render at every density.

Every fold opens to the whole on a tap, so this is folding by position rather
than dropping: every entry still renders, as the positions require, and a gate
decides a call, never a result, so no pending decision rests on a folded body.
Levels are not yet anchored against the scroll: folding an item above the
viewport while the reader is scrolled up moves the page — harmless while
following the tail, and the open item in backlog/web/thinking-autocollapse.md.

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

Call bodies truncate on the same budget results do, at both widths
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
  ([supervisor.md](supervisor.md)), so the chip states the spawning command
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

An assistant entry carrying `upstream_failure` is one the stream never stood
behind — an aborted partial, an absorbed HTTP failure, an empty finalized
stream — so `project` skips it and nothing in it is in the model's context
([transport.md](transport.md)). `session/tree.ts` emits a leading `:aborted`
leaf for it, above the content, which still renders because it is often the only
copy of what the model produced; the marker is warn-toned and rule-bordered, an
annotation rather than content. The node exists even when the entry is empty,
which is exactly the empty-stream case — otherwise that entry renders as
nothing at all. `upstreamFailureWhy` reads the prose out of either
serialization: the bare string every older log carries, or the object whose
`stream` field is the empty-stream evidence (which the transcript has no reason
to show and the inspector might). The words are the TUI's, deliberately:
*⚠ stream aborted — recorded, not in context*.

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
consolidation-mark notes as markdown (`react-markdown` + `remark-gfm`). Tool
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
blank line, since a single newline would soft-wrap the seam between them.

The renderer must produce a **React element tree, never an HTML string**. The
text is model-authored and the page holds a session credential, so a markup-
emitting renderer would mean `dangerouslySetInnerHTML` over untrusted content;
that constraint is what picks the library, not ergonomics. Two `react-markdown`
defaults carry it and neither should be switched off without reading
[backlog/web/untrusted-output.md](../backlog/web/untrusted-output.md): raw HTML
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
  distilled `read` is highlighted *and* keeps its per-line original line numbers.

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
the default set on `:root` — which is also the pre-script paint — and Lilac
overrides under `:root[data-theme="lilac"]`. `ui/theme.ts` resolves the
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
boundary without a one-sided rounded-corner mark. Drawer counts and checked
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
([backlog/web/plain-themes.md](../backlog/web/plain-themes.md) is the pending
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
(`ui/useFollowTail.ts`): the transcript scrolls the window, so "following"
means the viewport sits at the document's bottom — new entries and streamed
output keep it pinned there, scrolling up holds position, and a cold open
lands at the tail rather than the top of the log. **The preamble is not
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

**An `ask` reads as a turn ending** (`session/tree.ts`, `ui/Transcript.tsx`). A call named `ask` (docs/supervisor.md) with a usable `question` is not a call node: `buildTree` emits a leaf carrying the question and its place among the asks of one completion (`askOfCall`, the counterpart of `supervisor::ask_of_call`), drawn as the assistant's prose with a mark, after the step's other calls. The mark (`ui/askMark.ts`) says where the question stands: `answering` (above the prose in a softly lit current-question frame, with ` · question n of m` when `of > 1`) for the ask the live `ask` topic's `call_id` names, which is the one the composer's send answers; `next · question n of m`, dim, for an unresolved ask while the topic names a sibling; `asked` (with the position) once its result has committed (`AskQuestion.resolved`, set by `buildTree`) or when there is no topic to say more, as in a replay. While the topic is non-null the composer's placeholder reads `your reply answers question n of m`, or `your reply answers the question` for a lone ask. Its result entry becomes a second leaf at its log position: with a `submission_id` it is the reply, drawn as the operator's message from `raw.content` (attributed as a `User` turn is, which on this client is not at all); without one — an interrupt, a malformed call — a dim `not answered`. An ask with no usable question stays an ordinary call. A call whose arguments are still streaming is not drawn until it commits. The composer needs nothing for it: a send while the `ask` topic is non-null is the reply. Its echo is keyed like any other, and because the fold settles on any entry carrying the id, the reply's `ToolResult` settles it; the echo reads "Sent · answering the question" and carries no Recall, since a reply is never queued. (The daemon's refusal to continue past a question, `409 not_continuable`, reaches the composer's error line like the other continue refusals.)

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
Background-process output is the drawer's tasks section.

**Streaming flows into the committed state without a change of shape.** A call
whose arguments are arriving is the same item a committed call is — the intent
as its title (filling in as it streams, when it is the value in flight), the
signature, the bodies — and before anything structural arrives it is its bare
name. The live thinking tail is the pinned box; a long think that commits in the
recent tier stays in that box, stopped at its last line, with only its header
changing from "Thinking…" to "Thought · N words · show all".

**The consolidation mark** renders as a boundary: a dashed rule with
`⊙ consolidated through #N · M entries`, then the replacing summary as prose
between the rules. The count is committed entries at or below the boundary's
`upto` — client-computed from the window; the wire's token delta is provider
accounting the client does not hold. There is no expand affordance behind it:
render-whole means every entry the fold replaced is already on screen above,
so the mark's job is to say where the model's context stops carrying the raw
log, not to reveal anything. The `consolidate` call itself collapses to its
header once resolved — its input is the summary the mark renders in full
(the pair projects normally, so the log holds the summary twice), and its
result is a constant acknowledgement. A pending, gated, or failed
consolidate renders whole: the gate surface expects the call on screen, and
an error is information.

## Chrome and the content column

The page is full width; content sits in a centered `.col` (60rem). The chrome
— the top bar, the connection banner, the composer band, and the status bar —
bleeds to the viewport edges, and each bar's affordances anchor to the window
edges too: the wordmark and the drawer control, the turn state and the
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
context meter, and the **Session** control that opens the drawer. What the
session is — models, directory, cost — and how it runs are the drawer's: the
bar carried them on one line and could show none of them whole, and they are
not what the reader is doing. The title is the one element that yields width,
and on the phone breakpoint it is dropped: the start of a title says less than
nothing, and the drawer shows it in full. The **context meter** is the bar,
with `42.7k/1M` as a small caption under it so the numbers take no width of
their own. `ui/ContextBar.tsx` shares its fill with the switcher hover card:
occupancy is clamped to 0–100%, its halo grows and its core whitens with occupancy.
The rounded fill reaches cool white at full occupancy. Lilac has a continuous
colored rim and a wider hue cloud extending behind the caption; After Dark uses
the shared control halo. The hue moves
from the meter hue toward the accent between 50% and 85%, then toward the warn
hue between 85% and 100%. These are visual fill ranges, not consolidation
thresholds. Unknown usage draws no fill; an unknown window draws the count
without a bar. The **Session** control carries a count when anything in the drawer
asks for attention — pending notifications, policy proposals, running
processes (`ui/drawerAttention.ts`).

**The session switcher** (`ui/SessionSwitcher.tsx`) is a second row inside the sticky
header, present only under a host and only while another session is live. It lists the
*other* live sessions on the host, from the same listing subscription the overview holds
(`api/sessions.ts`), so it adds one connection at rest. A tab is a status dot and a title
(the id before one is written); the dot is read from the listing alone through `cardModel`:
a gated session is an accent dot with a halo, a session whose turn ended or that is asking
is the accent dot without one — a finished turn waits on the operator until the session is
archived — a stalled one is the warn colour, a running one pulses faintly, and a session
never started is a hollow ring. Hovering or focusing a tab opens a card with the overview
card's status line, title, activity line and context meter; that opens the session's own
state stream and log tail (`openSessionCard`) only while the card shows.
Tabs run most recently visited first, from visit times in `localStorage`
(`ui/switcherOrder.ts`), and

sessions never visited here follow in the host's id order. Tabs are measured against the
row, and those that do not fit are listed in a `+N` menu whose button carries an accent dot
when a session in it is waiting on the operator; attention does not reorder tabs. Entering
a session from a tab is a client-side navigation, as from a card.

It also carries one warn-tinted chip left of the meter, fed by the two
transport topics through `ui/upstream.ts` (a pure function of the pair, so the
component holds no endpoint or clock logic). While `upstream_retry` is set the
chip names the retry — `↻ attempt 2 of 12 failed upstream, retrying`, with a
`triage:` prefix for a triage peek's retry, and the upstream's `detail` text as the title;
otherwise `upstream_failure` names the last non-success status,
`upstream 429 ×3`, the count covering consecutive matching statuses and elided
at one. Both topics are level-triggered and both clear on recovery
(docs/transport.md), so the chip's absence is the all-clear rather than
"nothing observed yet", and a stalling loop's cause is visible while it
stalls. The chip renders no countdown — `next_attempt_at` is an absolute
instant, and a remaining-wait readout would mean either a clock read in the
render path or an interval driving a ticking value. Loop state is not restated
in the bar: the banner, the gate cue, and the composer's turn-state carry it.
The page is `viewport-fit=cover`, which draws it under the status bar and beside
the notch, so the bar pads itself by the `safe-area-inset-*` values; sticky
elements below it dock at its measured height rather than a constant.

## The composer area

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
interruption reason, not a resumable upload. Malformed saved data is ignored;
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
The other dials — review level, pace, hold — are the drawer's. The turn state is
"your turn" in the serif when the loop is idle, a dim `running…` while it
moves (`retrying the upstream · send to retry now` while an `upstream_retry`
pause runs; while a request or tool is open, the `current_operations` topic's
leading operation with its elapsed time on the client's clock — `waiting on
<model> · 2m 10s` before the first byte, `<model> streaming` after it, `triage peek
· waiting 40s`, `spotter judging · 3s`, `bash: <command> · 45s`, `retrying upstream ·
attempt 2 (429) · 8s`; the turn's own request leads, then a running tool, then the
side requests, in `ui/operations.ts`; a `bash` call is labelled by its `command`, cut
at 48 characters with an ellipsis, and its `intent` is the hover title, while other
tools are labelled by the intent), `waiting for your approval` or `waiting for
your answer` in muted, upright UI text while a gate or an `ask` is pending (it outranks
the operation text, since the operator is what the loop waits on; a stall and a
park still come first), and the daemon's own sentence plus `· send to retry` in the warn
color when the `stall` topic is set — a trajectory that stopped short, or a
triage gate held with the loop still running ([transport.md](transport.md)),
which `running` alone cannot say; the upstream's words ride along as the hover
title. An empty send is the continue, which retries either wait. The choice is
`ui/turnState.ts`, a pure function of `running`, `stall`, whether a retry
is waiting, a pending gate or ask, and the operations view (the topic, the model, and the clock), so the component holds no branching of its own. The span between state and controls is
reserved for the key-hint strip (binds are deferred to interactive
suggestions; [backlog/web/README.md](../backlog/web/README.md)). The input
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
input; an image's chip uses a local object URL while uploading and the stored
image URL once ready.
Removing one deletes the upload; one removed mid-upload is deleted when the
upload lands. Send waits for uploads to finish, carries every ready chip's
name, and removes the submitted ready chips only once the turn is accepted,
so a refused send keeps them. A draft of attachments alone is a turn, not the continue. The
paste and description rules are `ui/staging.ts`; draft and upload ownership are
`ui/drafts.ts`; the chips, staged and sent, are
`ui/Attachments.tsx`. A sent turn — and its pending echo — shows its
attachments under its prose, an image as a thumbnail served by
`GET /attachments/:name` that opens full size in a new tab.

**Sending an empty composer is the continue gesture.** With no draft, send
(and Enter, where Enter sends) issues `POST /v1/loop/continue`
([control-api.md](control-api.md)) — control handed back with no input, no
turn submitted, nothing appended — instead of the earlier silent no-op. A
`409 not_continuable` is a normal answer, so the daemon's `message` renders
in the composer's error slot on its own rather than as a thrown-error string.

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

A loop parked mid-turn at its cold check (the `park` topic) is `running` to the turn
state, so `turnState` reads it first and says `paused: cache expired · send to continue
(reads 120k tokens uncached)` in the warn colour; the composer shows the cold notice
with the same *summarize instead* button. After a fold the park stays and `cold` is
null; the readout says `paused: summarized · send to continue`, with no notice.

**Summarizing is the fold** (`POST /loop/fold`, [control-api.md](control-api.md);
`ui/fold.ts`, `ui/FoldNotice.tsx`, `ui/FoldControl.tsx`). Two entry points, one
rule (`foldOffer`): the cold notice's button, which runs the fold at once, and a
*Context* section in the drawer, available whether or not the session is cold,
which asks for confirmation first — a fold has no undo yet — saying whether it
spends a live cache. Both are held back, with the reason shown, while a gate or
question is pending or a warm loop is running; a loop parked at its cold check may
fold. Progress is the `fold` topic: the turn-state readout and the drawer say
`summarizing the history…`, a failure shows its error with a retry, and the
account lands as the consolidation mark in the transcript.

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
top bar. A non-`live` connection still shows as a full-width banner under
the bar, not just a dimmed word.

## The drawer

`ui/Drawer.tsx`: everything about a session that is not the conversation,
behind the top bar's **Session** control, so the page itself is the transcript
and the composer. A side sheet on a wide window; on the phone breakpoint a
bottom sheet with a grabber, rounded top corners, and room for the home
indicator. It closes with Done, a tap on the scrim, or history-back (it is an
overlay to `ui/overlayHistory.ts` like the inspector), and it and the inspector
exclude each other since both occupy the right edge. The slide is the
inspector's: transform only, entry from `@starting-style`, exit via `.closing`
with the unmount on `transitionend` and a timeout fallback.

**The phone sheet drags.** Its header is the handle; the sheet follows the
finger, stretching with resistance above its resting place. On release it
takes the velocity of the last 80ms only — a drag that sped up and then stopped
was not a flick — projects 180ms of that momentum, and closes if the projection
lands past half the sheet's height; otherwise it springs back. That is the rule
an iOS sheet follows; a quarter-height threshold with average speed closed on
short quick drags and left no way to abandon one.

The header is "Session details"; the title follows in full as a serif
paragraph. Sections, each drawn only when it has content — "no processes" and
"nothing pending" were placeholders a reader had to open to learn nothing
from:

- **inbox** and **tasks** first, because they are what asks for attention.
- **the session** — model, spotter, triage (only when it differs from main),
  directory, id, cost as `key value` facts; then the review level in words
  ("The spotter decides", "The spotter, then you", "You review every call",
  and "The supervising session" only while that is the value), the pause
  between steps, and hold after the current step — one `PATCH /v1/loop`, which
  the api layer's `patchLoop` translates to the wire's partial-`pace` shape.
- **policy**.
- **this device** — theme, density switches, notifications; app-wide rather
  than per-session, here until the app has a settings home.

Labels in the drawer are IBM Plex Sans (`--font-ui`), values mono, the title
serif: the interface voice is being tried here before the rest of the chrome
adopts it.

The sections carried over from the bottom panel they replaced, in detail
(`ui/Panel.tsx` holds them; `Drawer.tsx` arranges them):

- **tasks** — one row per process (command, pid, elapsed ticked once a
  second, muted flag) beside a detail pane: the command line, the verbs —
  mute/unmute, the background handoff with its optional note
  (`PATCH /v1/processes/:id`, a 202) on the foreground entry, kill
  (`DELETE /v1/processes/:id`) on any — and a live tail pinned to its
  bottom. The tail is `streaming.processOutput`'s rendering surface, the one
  streaming topic that folds into the store and renders
  nowhere. The mock's "foreground" verb is deliberately absent: the wire has
  a background handoff, not a foregrounding.

  The tail is **seeded, not only streamed**: `output` is ephemeral with no
  replay, so a process already running when the client connects has no deltas
  to build a tail from, and a backgrounded process is frequently quiet — which
  is usually why it was backgrounded. `session/processSeed.ts` reads
  `GET /v1/processes/:id/output` once per process as the `processes` topic
  first names it, and folds the result *under* whatever deltas have arrived
  since. An id with output already buffered is skipped: the retained read is a
  snapshot containing those same bytes, so seeding a warm buffer would
  duplicate them. Bytes the supervisor dropped at its retention cap are stated
  above the tail rather than silently skipped.
- **inbox** — pending notifications with dismiss
  (`DELETE /v1/notifications/:id`); on an idle loop the inbox delivers
  immediately, so rows only exist while a tool is mid-run.
- **policy** — the perimeter from `role` (scope, commands, paths, tools)
  with pending proposals merged *in their committed sort position*,
  ⊕-flagged, each carrying commit and cancel (`PATCH /v1/proposals/:seq`).
  The sort rule is the TUI's, and the reason travels with it: committing a
  proposal must not move it, because reflow at the moment of confirmation is
  how you lose track of what you confirmed. `ui/policy.ts` computes the
  rows, and `ui/policy.test.ts` pins the invariant (same index before and
  after commit). This closes the config-edit loop from this client: an agent
  that proposes a grant no longer blocks on a human at a terminal.

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
it. The drawer **floats over the content, which stays in place** — opening a
second view does not get to move the text someone is reading (the earlier
`body:has(.inspector)` margin yield did exactly that, and was reversed on
use). Its left edge is a drag-to-resize grip — width clamped
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

There is deliberately **no `file` tab**: the drawer
is call-scoped, so a tab here would read as "the file at the time of this
call", while a file read from disk is the file *now*, and serving the
historical reading would mean snapshotting every file at every edit. The
division held instead is that the inspector shows what a call did — from the
log, which holds it — and the file viewer shows what a file is
([backlog/web/file-view/](../backlog/web/file-view/README.md)). The `result` and `raw` tabs also carry **the command's own output**, under
whatever the committed result showed, for the two cases where a `ToolResult` is
not the output ([supervisor.md](supervisor.md)):

- **A call still running** has no committed result at all. Its live tail is
  already in the store as `streaming.toolOutput[callId]` — the transcript's
  inline peek renders it — and the drawer reads the same buffer instead of
  saying "no result yet".
- **A backgrounded call**'s committed result is the *handoff* result naming
  `p#<n>`, not the output; the output lives in the supervisor's retained
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
wire state, and the distiller's note — the audit view of
[redaction-fork.md](redaction-fork.md)'s output. Stepping between calls from
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

## The file viewer

`ui/FileView.tsx`: an overlay filling the band between the top bar and the
dock, showing what a file *is* — read from disk over `/files`
([web-bind.md](web-bind.md)) — where the inspector shows what a call *did*.
The chrome stays visible and working: the composer, the panel, and a pending
gate remain at hand while reading. The band's edges are measured
(`useChromeBand`, a `ResizeObserver` on `.top-bar` and `.dock`, the same
shape as the gate float's dock offset) because the dock's height moves; the
app column is `min-height: 100dvh` flex with the transcript growing, so the
dock sits at the viewport bottom even under a short transcript and the
band's bottom edge meets it. The slide-out passes behind the dock (both
carry a z-index, dock later in the DOM); the viewer sits below the inspector
and the gate float. The transcript stays mounted behind it and uses the
inspector's overlay machinery (`@starting-style` entry, `.closing` +
`transitionend` exit with the timeout fallback, `shown`-through-the-slide in
`App`); Escape closes. No router: the overlay is App state, like the drawer.

Entry is the `open ⤢` affordance: in the transcript it is the **path chip** on
the argument that names the file (above — a button, so the open-the-inspector
click ignores it), and in the inspector's footer it is a link per viewable file
the call names, labelled `open current file` — footer rather than tab, because
the drawer is call-scoped history and the file on disk is now. Which files a
call names is `session/touchedFiles.ts`: `write`'s `path`, the edit tools'
`file_path`, `read`'s `paths` — the inspector serves all three; the transcript
chips only the arguments the chip table names, which is the write-and-edit
half. **Within-cwd only, categorically** — an absolute or
`..`-leading path yields no affordance and is never followed; the call's own
log diff is its surface. That is a scope position (no arbitrary reads from
elsewhere on the system), not a resolution gap. The `session_identity` topic
([control-api.md](control-api.md)) now names the cwd, so it is the mapping
available if the scope ever widens.

**Highlights come from diffing successive fetches, never from the tool
call.** `session/lineDiff.ts` is the one diff engine: hand-rolled Myers over
lines, common affixes trimmed first, an edit-distance budget past which the
middle reports as one changed block. The bytes last rendered per path live
in a module-scoped map, so a highlight means *what changed since you last
looked* — identical for an agent edit and a save from emacs, and surviving
close/reopen of the overlay. A pure deletion has no line to tint and reports
as a seam (`removedAt`), drawn as a dashed rule on the line after it. Source
view tints lines (`NumberedCode`'s `lineClass`, `data-line` as the scroll
anchor); rendered markdown tints at **block granularity** — the same line
ranges mapped onto top-level blocks via hast `position` by a rehype plugin
stamping `data-changed` (`ui/Markdown.tsx`), sub-block precision
deliberately not wanted: the reader who wants lines toggles to source.

**Freshness is two sources through one mechanism.** A committed, non-errored
edit in the log (the `lastEdit` derivation) triggers an immediate
conditional GET; the daemon's change stream covers edits from outside — an
`EventSource` on the same `/files` URL (the content negotiation and the
watcher behind it are [web-bind.md](web-bind.md) "Watching for changes"),
held while the tab is visible and closed while hidden. Every poke, and
every (re)open of the stream — which is what covers changes missed while
hidden or disconnected, since `EventSource` reconnects on its own —
answers with the same conditional GET, so the stream carries no content and
replaced only the old 2 s poll's timer when it landed. A refusal drops the
held validator, because a file restored with its old mtime would otherwise
revalidate 304 against the refusal screen.

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

**Following the agent** is the same signal steering the view: an edit
landing on the open path re-reads it now; one landing elsewhere switches the
view to that file and lands on the edit. Edits and writes move the view; a
bare `read` never does — reads are the tree's working-set annotation instead.
The `following ⇢` toggle suppresses the switch and any jump the reader did not
ask for, while keeping the live reload and the highlights; a jump the reader
*did* ask for (a path chip, a tree click) happens either way.

**Refusals render legibly against the path** — "not a text file",
"too large to view", "no such file under the working directory" — never a
blank pane: the endpoint refuses rather than degrades, so every body that
arrives is whole, and there is no truncation state, windowed fetch, or
virtualization here. Markdown files default to rendered with a
`source`/`rendered` toggle; other files are highlighted source with
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
holds a directory-scope change stream ([web-bind.md](web-bind.md)) so what is
on screen stays current. As with the viewer, the answer to stream *open* is the
same re-list as the answer to a poke, which is what covers a change landing
during stream setup. The root is expanded at mount.

The **finder** (`session/pathMatch.ts`) is a palette over one recursive
listing, fetched the first time it opens. Subsequence matching with smart case;
three things earn points — a hit inside the basename, consecutive matched
characters, and a match at a segment boundary — and shorter paths break ties, so
the file beats the vendored copy of it. Searching file *contents* is not built:
path matching is a listing the client already has, where content search is a
server-side scan with its own cost, ranking, and result-rendering questions.

The **annotations are derived client-side** from the transcript the store
already holds (`fileActivity`, `session/touchedFiles.ts`); the endpoint
contributes nothing and knows nothing about the session. Files the session has
read are the agent's working set and read bold; files it has changed take the
accent. This is where a read registers at all, since reads deliberately do not
move the viewer.

Entry is the viewer's `files ⌸` toggle, or the `/tree` route — the one client
route besides `/`, which opens the viewer on the working directory with no file
chosen. Both routes are named in `SPA_ROUTES` (`src/daemon/tcp.rs`), which is
also what the service worker's offline fallback mirrors.

The listing's ignore set is gitignore-derived and lives on the server
([web-bind.md](web-bind.md)); a file git cannot see is not discoverable here,
while remaining readable and followable by name.

## Installability

`public/manifest.webmanifest`, `public/sw.js`, and the icons: the app installs
to a home screen and runs in `standalone` display. The icon is the wordmark's
own glyph — the `e` from Newsreader, the face the top bar speaks in — on the
After Dark page background, as an SVG (`public/icon.svg`, the source) and the
PNG sizes derived from it. `theme-color` starts at After Dark's page background
and follows the resolved theme (`ui/theme.ts`), since the standalone window's
chrome takes it.

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
hash is the version) and the shell routes. Registration
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
spotter's reason, into which the surface portals its controls. Reading order is call →
reason → decision, all in one box: the decision is visibly *about* the call rather than a
panel following it. The reason is the `Spot` verdict's text alone, with no stance label —
the box and its controls already say what is being asked — and only the waiting call
draws one: on a decided call the outcome is its result, and the verdict is the
inspector's. The controls are **Allow** and **Decline…**, which opens the redirect
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
the call renders a second time (via the same `CallBlock`/`SpotLine` the
transcript uses, under the transcript's own classes: intent, full command,
verdict), because the entries the in-flow surface leans on are exactly what is
off-screen. It is drawn as the gated entry lifted: the entry's fill and
border, at the entry's measured left edge and width, plus a shadow. The panel shares
the in-flow surface's form state — one set of drafts, rendered in both places —
so a decision composed in one survives into the other. It collapses, without
resolving the gate, to a compact cue naming the call (it may be covering text
the operator is reading); the cue expands it back, and it re-expands for each
new call. Two properties are deliberate: the floating surface **never scrolls
the page on its own** — a pending decision does not get to move the page out
from under someone reading — and its `jump to call ↓` is a jump to the document
tail (the gated call has no result, so the gate *is* the tail), not a smooth
`scrollIntoView`. `useFollowTail` pins to the bottom after every render while
the viewport is near it, so an animated scroll toward the tail re-engages the
follow partway down and is cancelled by the next render. Any future
scroll-to-somewhere affordance meets the same constraint.

The decisions ride `PUT /v1/approvals/:call_id` (`Decision` in `api/types.ts`):
approve, abort, and redirect with steering text. The route also takes a
`config_edit` grant, which the web client does not offer: a grant guessed from
the call's input was a command prefix whatever the call gated on, so
"always allow" waits for the host to propose the grant itself
([../backlog/permissions/gate-grant-proposal.md](../backlog/permissions/gate-grant-proposal.md)).
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
(`SecurityHeaders` in `src/daemon/tcp.rs`), alongside `x-content-type-options:
nosniff`, `referrer-policy: no-referrer`, and `x-frame-options: DENY` (redundant
with `frame-ancestors` for anything current, and one header for browsers that
predate CSP3). It sits outermost in the handler
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
`style=""` attributes parsed from markup rather than CSSOM mutations. Under
`--features dev-proxy` the page is Vite's — an inline module preamble and an HMR
socket — so the policy relaxes, gated on the same `cfg` as the proxy so a release
build cannot serve it. A test asserts the shipped policy contains no `unsafe-`.

CSRF is a different threat and is already answered: the session cookie is `Secure`,
`HttpOnly`, `SameSite=Strict`, `Path=/` ([web-bind.md](web-bind.md)). A CSRF token
would not help against injection — script running in this origin can read one out
of the DOM.

## Following a hand-off

A session that hands off (docs/tools.md) names its successor in `session_identity`, and
`SessionView` replaces the page's location with the successor's (`ui/identity.ts`
`successorHref`) — `replace`, so Back does not return to the archived predecessor. The
successor's transcript opens on the predecessor's brief, whose `User` entry carries the
reserved `handoff[<id>]` source (`api/types.ts` `handoffPredecessor`): drawn as a harness
note ("handed off from `<id>` — its brief:") rather than as the operator's turn.

## Serving and auth

The gate enumerates its anonymous surface rather than deriving it from guarded
prefixes ([web-bind.md](web-bind.md)): the app shell and hashed assets serve without a
credential — the same posture as the inline bootstrap page the app replaced — as does
`POST /auth/session`, and everything else, `/v1` included, takes cookie-or-bearer. The
login view (`web/src/ui/Login.tsx`) carries the bootstrap dance: read `#token=` from
the URL fragment, trade it at `POST /auth/session`, scrub the fragment; render the
six-digit field on a bare visit or a failed trade. The SPA index answers the client's
named routes and nothing else (`SPA_ROUTES`, `src/daemon/tcp.rs`) — a broader index
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

- `cargo run --features dev-proxy -- host --bind …` — the daemon spawns Vite and
  proxies to it; HMR works through the gate, because that build widens the
  anonymous surface to Vite's module URLs (`DEV_PROXY_PREFIXES`,
  `src/daemon/tcp.rs`) — the shell has to load before anything can authenticate.
- Without the feature, the build embeds: the `frontend!` macro runs `vite build` at
  compile time, so building the crate needs `pnpm` on PATH and `web/node_modules`
  installed (`pnpm install` once).
- `pnpm check` in `web/` — typecheck, lint, format check, vitest.
- `pnpm dev` standalone also works: `web/vite.config.ts` proxies `/v1` and `/auth`
  to a daemon on `:7878`.

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
buttons (including the Session drawer's attention badge), neon glyphs, session
tabs and their hover card, composers, code/results, images, session cards, launch
settings, the model picker, and resume lists. Specimen labels stack above their
controls on narrow screens; state comparisons wrap rather than widening the page.

`Scenes.tsx` renders a shortened worktree UI exchange in finished, input-arriving,
working, answer-needed, and approval-needed states. Input-arriving shows an
unfinished single-line scalar argument to exercise transient wrapping. It uses
`TranscriptContent`, the
production transcript body without window follow-tail, built from fixture log
entries through the normal tree projection. Tool output includes the distilled
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

The composer fixtures share `ComposerToolbar` and `turnState` with the app;
`DrawerToggle` is shared with `TopBar`. Code/result specimens use the production
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
that small is inlined as a `data:` URI, and the client's CSP (`default-src
'self'`, no `img-src`; `src/daemon/tcp.rs`) blocks those. The gallery mounts
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
([../backlog/session-replay.md](../backlog/session-replay.md)).

The whole module sits behind `import.meta.env.DEV` in `useSession.ts`, which a
production build folds to `false`; the branch and the module go with it.
