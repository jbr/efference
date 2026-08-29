# The web client

A React single-page app served by the daemon's TCP bind ([web-bind.md](web-bind.md)):
the second human client after the TUI, and the mobile client for now. Sources live in
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
| `web/src/api/` | mirrored `/v1` types (`types.ts`), the control base (`base.ts`), the fetch wrapper + error envelope (`http.ts`), the SSE reader (`stream.ts`) | no |
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
daemon at a socket root and a host at a prefix ([host.md](host.md)) are then the
same client code, and nothing above `api/` names a binding. `/auth/…` and
`/sessions` are deliberately *not* joined: they are the listener's own surfaces,
at its root under either binding — one credential covers every session behind a
host.

The id set the derivation accepts mirrors the host's (`plain_id`, `src/host.rs`):
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

**The list is the ids and nothing else** (`ui/SessionList.tsx`), because the listing
is the ids and nothing else. Model, stall, and gate chips per row would be one
`GET /sessions/<id>/v1/state` per session on every load — acceptable at three
sessions, and the wrong shape at thirty; the cross-session summary the list actually
wants is a host-level resource that is not built ([host.md](host.md)).

**Entering a session is a navigation, not client-side state.** A row is an `<a href>`
to `/sessions/<id>/`, and the back affordance in the top bar is a link to `/`, shown
only when the base is non-empty. The base changes with the URL, so a client-side
transition would have to rebuild every fetch, the stream, and the fold against a
different endpoint — which is a page load with extra steps. The one client route
besides `/` is `/tree`, and it is `/sessions/<id>/tree` under a host: the same route
with the base in front of it, which is why the route is read with the base removed.

`ui/App.tsx` is only this choice — list, login, or session. The session view is
`ui/SessionView.tsx`, which is the component the rest of this document means when it
says App holds a piece of state (the inspector selection, the open file request, the
overlay count).

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
- Chrome rows tighten: the status bar wraps instead of clipping the dials off
  the right edge, the usage meter narrows, the upstream chip ellipsizes, and
  the gate's decision rows wrap so `allow prefix` stays on screen (verified
  live: a `human_only` gate decided by tap from the simulator).
- **The chrome quiets.** On a phone every non-transcript pixel is clutter, so
  at rest the dock is one row — the status dot and turn state — with the
  dials and the (empty) panel tab strip hidden behind the row's `⋯` toggle
  (App state, ephemeral on purpose: a reveal is a moment, not a mode).
  Signal always shows through: `interrupt` stays whenever the loop runs, a
  held pace stays because a hidden hold reads as a hang, and the tab strip
  reappears on its own whenever any tab carries a count or a pane is open.

**Overlays are history-backed** (`ui/overlayHistory.ts`, both form factors):
each open overlay — inspector, file viewer — owns one history entry, so the
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
  rendering (`kept N of M lines` in the header). When nothing was dropped
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
├─ user text / thinking / agent text / mark / ambient / config-edit / preamble
└─ batch                    the calls issued in one step
   └─ call                  one call, whole
      ├─ intent, args
      ├─ spot               the spotter's stance + reason
      ├─ telemetry          duration, outcome, raw result size
      └─ result             carries its own keep + note
```

Two containers, `batch` and `call`; everything else is a leaf sibling at session
level. A result, a verdict, and a call's telemetry are attached to the call they
name rather than emitted where they fall in the log — that attachment is the
point, and it is what makes a result *belong to* its call instead of merely
following it.

`ToolTelemetry` ([tools.md](tools.md)) joins by `call_id` exactly as `Spot` does,
and for the same reason: it is a fact *about* a call, not an entry to read. The
default for a payload the tree does not recognise is a leaf rendered as `unknown
entry`, which is the right default — a new variant is visible rather than
silently dropped — but it means every additive log variant needs a decision here.
`ToolTelemetry` shipped without one and put a raw JSON block after every call.

A result's images render below its body as plain `<img>` elements pointed at
`GET /v1/log/{seq}/images/{idx}` ([control-api.md](control-api.md)): the entry
view carries only `{media_type, width, height}` — the attributes that hold the
aspect box before the bytes land — and the browser supplies the fetch (ambient
session cookie), the immutable caching, and lazy loading. Capped at 420px
display width; full resolution is the browser's own open-image-in-new-tab.

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
their labels, because those genuinely need naming. The earlier failure mode —
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

The pre-commit tail renders flat rather than inside a batch: nothing there has
committed, so its calls have no results to contain and no verdicts to carry, and
the committed entry supersedes the whole set at once.

## A call reads as a signature

Every call renders as pseudocode — `render/parse.ts`'s `callView` computes it,
`ui/CallHead.tsx` renders it:

```
ferritin.search(crate: "serde", query: "Deserialize")
check_inbox()
write(path: "src/main.rs", overwrite: true, contents: 505 lines)
```

The form is **total and schema-free**, which is the point: MCP servers are
user-mountable and their argument shapes are arbitrary, so a tool this client
has never heard of must render as well as a built-in. Three rules carry it, and
they replaced a per-tool table plus a pretty-printed JSON envelope for
everything not in it — which is how `check_inbox` came to render as a code block
containing `{}`.

- **The name is the dotted one.** `<server>.<tool>`, never the wire's
  `<server>__<tool>`: the dotted form is the only spelling an operator ever
  writes (an eval config's `tools = ["ferritin.get"]`), so showing the wire form
  showed a name that appears nowhere else in the product. `splitToolName`
  mirrors `mcp::display_name` (the boundary is the *first* `__`) and is what
  `ui/policy.ts` dots the policy tab's grants with too. The server segment is a
  dimmed span of its own rather than a smaller one — built-in versus
  mounted-server is the `open_world_hint` distinction, i.e. whether the call
  could have reached outside the session, which is worth a standing visual.
- **Inline versus body is one rule for all tools.** An argument sits in the
  signature when it is scalar, single-line, and inside a 48-character budget —
  strings quoted, every other scalar bare, a collection as compact JSON while
  that fits. Otherwise it is elided *to its shape* there (`505 lines`,
  `640 chars`, `[3 items]`, `{2 keys}`) and rendered as a block below, labelled
  with its key. So "which argument is the body" is decided by what did not fit,
  not by a per-tool table, and a third-party tool shaped like a file write needs
  no special knowledge. Inline arguments sort ahead of elided ones (stable
  within each group): they are the call's identity, and a 505-line shape between
  two of them buries it.
- **Empty is empty.** A no-argument call is `check_inbox()` — no block, no `{}`.

Call bodies truncate on the same budget results do, at both widths
(`ui/truncate.ts`: 12 lines at desktop, 4 on the phone) — which is what stopped
a `write` from printing the whole file into the transcript.

Two tools stay special, and only two. **`bash` renders its command bare** —
quoting a shell line into `bash(command: "…")` reads worse than the line, and
only a command containing a newline becomes a block. **The edit family signs its
arguments but emits no body**: the diff rendered below the head is the
representation there, so a body would show the change twice; the inspector's
args tab (`callArgs`, a separate computation) is where the whole input is asked
for. The cases the signature *replaced* — `read`, `search`, `list`,
`read_image`, `write` — were each an inline gloss the generic rule now produces.

**The TUI renders the same form** (`src/tui/projection/tools.rs`'s `call_view` /
`call_spans`; [tui.md](tui.md)), so the two clients agree about how a call reads;
this section is the shared statement of the shape, and the Rust does not restate
the rules. Two things differ, both from the data rather than by choice: the TUI's
arguments read in **sorted key order**, because a committed call's input is a
`serde_json::Value` whose object keys sort for the wire's byte stability, where
this client parses JSON and keeps the model's emission order; and the TUI does
**not** truncate call bodies, because it truncates nothing — the budget below is
the web's, matching what it does to results.

Tool **descriptions** stay out of the transcript. They are static per tool, so
in a run with 55 `ferritin.*` calls the same sentence would render 55 times,
while the call's own model-authored `intent` — already a sibling line — says
what *this* call was for. The inspector is where a description is paid for once.

The head's markup carries the wrapping rule. It is a plain block holding one
inline flow, `.call-sig`, whose children are ordinary inline spans and wrap as
text at every width; `overflow-wrap: break-word` lives on that item, so a
multi-path argument breaks between paths and only ever mid-token when one token
is wider than the line. It was a flex row for as long as an affordance sat at
the end of it, and flex items are blockified by the CSS Display spec — so a
signature built of sibling spans wrapped as *whole boxes* onto their own lines,
which is why a long single-line `bash` command used to read as a block. With the
`open ⤢` slot absorbed into the argument it referred to (below) there is nothing
to place at the end of the row, and the whole head is one sentence again.

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
- **Tokens, not HTML.** `codeToTokens` returns `{ content, color }[]` grouped by
  line, which the components render as spans. `codeToHtml` would return a markup
  string, and nothing on this page turns untrusted text into markup. The line
  grouping is also the shape the numbered-gutter view already wanted, so a
  distilled `read` is highlighted *and* keeps its per-line original line numbers.

Highlighting is best-effort: until the grammars load, and permanently if they
fail to, every caller renders the plain text it did before. The one rule carried
from the TUI is that a block is highlighted **only when its language is known** —
the TUI's renderer used to default to `rust`, which was right in ferritin and
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
the default set on `:root` — which is also the pre-script paint — and Daylight
overrides under `:root[data-theme="daylight"]`. `ui/theme.ts` resolves the
choice — explicit and persisted in localStorage, or the system scheme when none
is stored — and sets `data-theme` on `<html>`; `initTheme()` runs before first
render, and the select lives in the bottom panel's interface tab. Glow is a token,
not a decoration: After Dark's accent halo (`--accent-glow`) goes `transparent`
in Daylight, and every box-shadow through it disappears with it. Adding a theme
is authoring an override block plus a select option
([backlog/web/plain-themes.md](../backlog/web/plain-themes.md) is the pending
pair).

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
assets as `data:` URIs, and the CSP has no `data:` source. Daylight's
paper-grain SVG therefore lives in `public/` (never inlined, served as-is),
not `src/assets/` — anything else CSS references by URL follows the same rule
or grows a hash in `default-src`.

## Transcript ergonomics

Two behaviours over the render-whole transcript. **Follow-tail**
(`ui/useFollowTail.ts`): the transcript scrolls the window, so "following"
means the viewport sits at the document's bottom — new entries and streamed
output keep it pinned there, scrolling up holds position, and a cold open
lands at the tail rather than the top of the preamble. **The preamble
collapses** to a native `<details>` line that expands on tap — presentation
of one audit entry, not a fold affordance; render-whole for tool results
stands (the positions above).

**Density toggles** (`ui/viewSettings.ts`, persisted like the theme): three
switches hide thinking, clear spot verdicts, and ambient entries. Hiding is
display-only — the entries stay in the store and reappear on toggle — and a
verdict that gated or watched always renders; only the routine `clear` hides.

**The pending-turn echo.** A submitted turn the daemon accepted (a 202)
renders immediately as a dimmed, `⋯ pending`-marked user turn after the live
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

Each echo carries a **recall** affordance: `DELETE /turns/:submission_id`
withdraws the pending-queue suffix beginning at that turn and answers with what
it actually popped (docs/control-api.md). Nothing was ever logged, so there is
nothing to retract. The action drops only the echoes the daemon reported
popping, and hands the texts to the composer, which prepends them to the draft
already there joined with a newline — via a `{text, nonce}` prop, because the
composer owns its draft and recalling the same text twice is two events rather
than one value. An empty answer is the lost race — the loop dequeued the turn
first — and the draft is left alone.

**The live-output peek.** A running tool's streamed output shows as the last
10 lines, dimmed, under a `⋯ running · N lines` header — a glance at a live
process, not a truncation: the whole output arrives, whole, when the call
commits. The one exception to render-whole, carried from the TUI
([tui.md](tui.md)). Background-process output is the bottom panel's surface,
not the transcript's.

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
— the top bar, the connection banner, the dock's tab strip, composer band, and
status bar — bleeds to the viewport edges, and each bar's affordances anchor
to the window edges too: the wordmark and usage meter, the panel tabs, the
turn state and loop dials (full width for parity across the bars,
2026-08-20). Only content keeps the column: the transcript, the panel panes,
the composer input. Chrome is also invisible to text selection (`user-select: none`
across labels, bars, tabs, and controls) — only content selects: transcript
text, commands, results. Inside the column, entry content carries a right
inset balancing the label gutter's air, so text in a block does not run flush
against the column edge.

## The top bar

`ui/TopBar.tsx`: sticky interface chrome — the serif wordmark, the session
status dot (breathing while the loop runs), and the usage meter: context
occupancy as numbers and a thin bar, plus the session cost. It also carries one
warn-tinted chip left of the meter, fed by the two transport topics through
`ui/upstream.ts` (a pure function of the pair, so the component holds no
endpoint or clock logic). While `upstream_retry` is set the chip names the
retry — `↻ stream aborted, retrying (2 of 12)`, with a `peek:` prefix for a
fork peek's retry, and the upstream's `detail` text as the title; otherwise
`upstream_failure` names the last non-success status, `upstream 429 ×3`, the
count covering consecutive matching statuses and elided at one. Both topics
are level-triggered and both clear on recovery (docs/transport.md), so the
chip's absence is the all-clear rather than "nothing observed yet", and a
stalling loop's cause is visible while it stalls. The chip renders no
countdown — `next_attempt_at` is an absolute instant, and a remaining-wait
readout would mean either a clock read in the render path or an interval
driving a ticking value. Deliberately minimal otherwise — the mock's bar is a
dumping ground of imagined status items, and additions happen one argued
element at a time. Loop state is not restated here: the dot breathes, and
the banner, the gate cue, and the composer's turn-state carry the rest.
Beside the dot it names **what the session is** — the model and the working
directory, from the `session_identity` topic
([control-api.md](control-api.md)). Which model is answering is not derivable
from the transcript, and it is what a reader needs when cheap and flagship
models mix across sessions. The wire carries the cwd absolute; `ui/identity.ts`
shortens it to `~/code/efference` by recognizing `/Users/<name>` and
`/home/<name>` as whole components — a browser cannot read `$HOME`, and a
wrongly shortened path would name a directory that does not exist, so anything
unrecognized shows verbatim (the full path is the element's title either way).
Both shrink with an ellipsis rather than pushing the meter off the bar, and on
the phone breakpoint the directory drops while the model name stays.

## The composer area

The loop dials live on the dock's status bar (`ui/Composer.tsx`): gates
arrive at the composer, so its dock is where current state is controlled.
Interrupt (`POST /v1/loop/interrupt`, enabled while `running`), hold/resume,
the pace select, and the review-level select — one `PATCH /v1/loop` for the
dials; the api layer's `patchLoop` translates to the wire's partial-`pace`
shape. The status bar is the dock's **outermost row, below the composer at
the window edge**, full-bleed with the turn state at the far left and the
dials at the far right — chrome frames content, as in a native status bar
(the row originally sat *above* the input, which put the interactive content
at the window edge; reversed on use, 2026-08-20). The turn state is a
breathing accent dot beside "your turn" in the serif when the loop is idle,
a dim `running…` while it moves, and the daemon's own sentence in the warn
color when the `stall` topic is set — a trajectory that gave up rather than
finished ([transport.md](transport.md)), which the idle state alone cannot
say; the upstream's words ride along as the hover title. The three-way choice
is `ui/turnState.ts`, a pure function of `running` and `stall`, so the
component holds no branching of its own. The span between state and dials is
reserved for the key-hint strip (binds are deferred to interactive
suggestions; [backlog/web/README.md](../backlog/web/README.md)). The input
carries no hint about mid-trajectory injection
([control-channel.md](control-channel.md)): the turn-state indicator beside
it already says whose turn it is, and riding along is what sending out of
turn does, so naming the mechanism cost prime UI space without adding
anything the operator acts on (a hint shipped here and was removed
2026-08-26).

**Sending an empty composer is the continue gesture.** With no draft, send
(and Enter, where Enter sends) issues `POST /v1/loop/continue`
([control-api.md](control-api.md)) — control handed back with no input, no
turn submitted, nothing appended — instead of the earlier silent no-op. A
`409 not_continuable` is a normal answer, so the daemon's `message` renders
in the composer's error slot on its own rather than as a thrown-error string.

The input is inset into the dock rather than floating on it: the composer
band shares the dock's surface color, and the box is near-square (2px) in
the chrome's flat language. The field auto-grows with the draft
(`field-sizing: content`, 3–10 lines), replacing the manual resize grip; a
`1ch` right padding gives the block caret its cell at the wrap column, where
zero padding clipped it to a sliver. A `contenteditable` composer (live
markdown styling) is an anticipated replacement for the textarea; nothing
here should grow attachments to the textarea being one.

The controls strip is gone; its last resident, the usage line, lives in the
top bar. A non-`live` connection still shows as a full-width banner under
the bar, not just a dimmed word.

## The bottom panel

`ui/Panel.tsx`: tasks, inbox, policy — docked with the composer (`.dock`,
one sticky footer), collapsed to its tab strip by default, the open tab
persisted like the theme, counts riding the tabs.

- **tasks** — one row per process (command, pid, elapsed ticked once a
  second, muted flag) beside a detail pane: the command line, the verbs —
  mute/unmute, the background handoff with its optional note
  (`PATCH /v1/processes/:id`, a 202) on the foreground entry, kill
  (`DELETE /v1/processes/:id`) on any — and a live tail pinned to its
  bottom. The tail is `streaming.processOutput`'s rendering surface, the one
  streaming topic that previously folded into the store and rendered
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
- **interface** — the theme select and the transcript density switches,
  buried on purpose: neither is an affordance used *during* a session, so
  they do not merit constant display (moved out of the header, 2026-08-19).
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
`ResizeObserver` rather than guessed in CSS — the panel and the composer
both change that height.

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
`callView.body`. Serving both surfaces from one value is what made the tab
say "no body — the header says it all" on every edit, which was false: the
header shows `file_path · operation`, while `anchor` and `content` — the
substance, a median 752 bytes of input across the corpus — went unshown.
`callView.body` stays null for the diff family because the diff below the
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

The service worker does two things and no third: make the app installable, and
let the shell paint from cache on a cold start or a lost network. **Nothing
session-shaped is cached** — `/v1`, `/files` and `/auth` always reach the
daemon, matched with any session prefix removed so the rule covers both
bindings; the shell routes are matched the same way, so a session-prefixed
navigation falls back to the same cached shell. A transcript or a file's bytes served from cache would be a stale claim
about what the agent did, and saying what the agent did is what this page is
for. What is cached is the build's own hashed assets (cache-first, because the
hash is the version) and the shell routes. Registration is production-only
(`src/pwa.ts`); on the dev path it unregisters instead, since a cached asset is
what HMR must not get. Push is the other half and needs a durable home for
subscriptions, which is the host's.

## The gate surface

The pending approval gate (`web/src/ui/Gate.tsx`) renders from the `gate` state
topic, in the transcript flow above the composer rather than a modal, and is
**the affordance only** there. The call it decides is already on screen: a gated call
has no result entry, so that call and its `Spot` verdict are the transcript's
last two entries, directly above. Reading order is therefore call → verdict →
affordance, from entry order alone, as in the TUI ([tui.md](tui.md)). The
surface rendered the call itself once, through `JSON.stringify` — which put a
`write`'s file body on screen as one escaped line, and an `edit`'s `anchor` and
`content` likewise, on the one surface where a human commits. Rendering it a
second time through the cordon's `callView` would have been readable and still
duplicated the whole body against the `CallBlock` above it; not rendering it is
what removes both. The `gate.reason` fallback survives for a gate held with no
`Spot` entry behind it — a window the stream's coherence rule closes
([control-api.md](control-api.md)) — so it is robustness rather than a path that
normally renders.

Because the affordance sits in the flow rather than pinned, it can be scrolled
away from. While an `IntersectionObserver` reports the in-flow surface out of
view — or the file viewer is open, which occludes the surface without moving
it out of the observer's geometric view (the `covered` prop from `App`; the
float renders above the viewer, its jump-to-call link dropped because the
tail it would land on is behind the viewer) — a **floating gate panel** pins
above the dock — and it is the one place
the call renders a second time (via the same `CallBlock`/`SpotLine` the
transcript uses: header, intent, full command, verdict), because the entries
the in-flow surface leans on are exactly what is off-screen. The panel shares
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

All four decisions ride `PUT /v1/approvals/:call_id` (`Decision` in `api/types.ts`):
approve, abort, redirect with steering text, and allow-with-prefix (a
`config_edit` carrying a `command` change, plus an optional note). The prefix
input prefills from the verdict's `probed_prefix` — the candidate the spotter's
promotion question named ([permissions.md](permissions.md)), so the operator
ratifies the same prefix the spotter answered for — falling back to the call's
whole `command` field (allowing exactly this command) when no probe rode the
call. A `409 gate_moved` is handled in
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

**A `Content-Security-Policy` on every response the bind produces**
(`SecurityHeaders` in `src/daemon/tcp.rs`), alongside `x-content-type-options:
nosniff`, `referrer-policy: no-referrer`, and `x-frame-options: DENY` (redundant
with `frame-ancestors` for anything current, and one header for browsers that
predate CSP3). It sits outermost in the handler
stack so its `before_send` runs last, and the headers land on the SPA index, a
hashed asset, a `/v1` rejection and a 404 alike — a handler that answered without
them would be the hole. The stack is built once by `stack()` and used by both the
listener and the tests, because a hand-copied test stack is what lets the two
drift.

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

## Serving and auth

The gate's anonymous surface is every GET outside `/v1` plus `POST /auth/session`
([web-bind.md](web-bind.md)): the app shell and hashed assets serve without a
credential — the same posture as the inline bootstrap page the app replaced — and
`/v1` stays behind cookie-or-bearer. The login view (`web/src/ui/Login.tsx`) carries
the bootstrap dance: read `#token=` from the URL fragment, trade it at
`POST /auth/session`, scrub the fragment; render the six-digit field on a bare visit
or a failed trade. The SPA index answers the client's named routes and nothing else
(`SPA_ROUTES`, `src/daemon/tcp.rs`) — a broader index fallback would answer scanner
probes `200`. On the host those routes are also answered under a session prefix,
which is the one place the anonymous shell sits inside a guarded prefix
([host.md](host.md)).

## Development

- `cargo run --features dev-proxy -- serve --bind …` — the daemon spawns Vite and
  proxies to it; HMR works through the gate (upgrades are GETs outside `/v1`).
- Without the feature, the build embeds: the `frontend!` macro runs `vite build` at
  compile time, so building the crate needs `pnpm` on PATH and `web/node_modules`
  installed (`pnpm install` once).
- `pnpm check` in `web/` — typecheck, lint, format check, vitest.
- `pnpm dev` standalone also works: `web/vite.config.ts` proxies `/v1` and `/auth`
  to a daemon on `:7878`.

### Scrubbing the stream — `web/src/session/record.ts`

A dev build interposes a recorder between the stream reader and the fold.
`connect` takes an optional `FrameTap`; the recorder's keeps every frame on a
tape in arrival order and decides when the store consumes it. The connection is
otherwise unchanged — same catch-up read, same `GET /events`, same frames — so
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
DOM and nothing else, so a global the page set is invisible to them. A fixed
corner badge names the mode and position, without which two screenshots at
different cursors are indistinguishable.

The tape is in memory and covers the current session: a reload starts a new one,
and so does an HMR update to a `session/` module. Repeatability across reloads
would mean persisting the tape, or serving a recorded session through the
daemon's own code — the latter being the only way the TUI gets it too
([../backlog/session-replay.md](../backlog/session-replay.md)).

The whole module sits behind `import.meta.env.DEV` in `useSession.ts`, which a
production build folds to `false`; the branch and the module go with it.
