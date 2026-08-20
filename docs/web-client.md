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
| `web/src/api/` | mirrored `/v1` types (`types.ts`), the fetch wrapper + error envelope (`http.ts`), the SSE reader (`stream.ts`) | no |
| `web/src/session/` | the fold: frames → an immutable `Session` snapshot (`store.ts`), the connect lifecycle (`connect.ts`), session verbs whose error recovery folds into the store (`actions.ts`), one `useSyncExternalStore` binding (`useSession.ts`) | binding only |
| `web/src/ui/` | display — transcript, login, composer; the churn zone | yes |

`types.ts` is the one place wire shapes are declared client-side; nothing in `ui/`
names a wire field. The fold in `store.ts` is pure functions over plain data —
`applyFrame(session, frame)` — with vitest coverage of the properties the API doc
promises (idempotent log replay, whole-topic state replacement, buffer settlement
when an entry commits). When the control API moves, `api/` and the fold are the
whole blast radius.

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
  tools, `read`'s sentinel stripping and extension→language map, per-tool
  call summaries with the `intent` leading the entry, and the distilled
  composition — the parse runs over the whole raw, the verdict's kept line
  numbers filter the rows it produced, so a triaged view keeps its tool's
  rendering (`kept N of M lines` in the header).

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
      └─ result             carries its own keep + note
```

Two containers, `batch` and `call`; everything else is a leaf sibling at session
level. A result and a verdict are attached to the call they name rather than
emitted where they fall in the log — that attachment is the point, and it is
what makes a result *belong to* its call instead of merely following it.

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

## A call's input, while it arrives

While a call's input streams, the accumulated buffer is a *prefix* of a JSON
document. `render/partial.ts` (mirroring the TUI's `projection/partial.rs`,
[tui.md](tui.md)) splits it positionally: within an object only the last key can
be incomplete, because the bytes for the earlier ones arrived and closed. The
settled pairs go through the same `callView` a committed call renders with — so
the header fills in (`calling write · src/api.rs…`) as bytes land and the block
keeps its shape when the entry commits — while the pair still arriving renders
only as content: a multi-line string as a code block in the language of whatever
*settled* path names it, a single-line one as a dim `key: value…` line. A
half-arrived value never backs an identity claim (`write src/api.r` is
well-formed, plausible, and false), and before anything structural arrives the
raw characters show, which claim nothing.

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

Thinking as prose is a default with a **named reversal trigger**. The measured
corpora (322 blocks across two Claude models and two harnesses, 2026-08-18)
are uniformly blank-line-separated paragraphs whose one markdown construct is
inline code — for that shape, markdown renders the backticks and can mangle
nothing. A model whose thinking is line-structured — bare adjacent lines that
markdown would merge into one paragraph — is the sighting that flips thinking
back to preformatted (or to markdown-with-hard-breaks, which serves both
shapes). The TUI still renders thinking preformatted
(`DocumentNode::Lines`, [tui.md](tui.md)). A mark's note has no such caveat:
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
- **The JavaScript regex engine**, not the Oniguruma WASM one — no `.wasm` to
  ship, and no `wasm-unsafe-eval` needed in the CSP the untrusted-output pass
  will add.
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
lives on the session (`pendingTurns`), pushed by the submit action after the
202 and settled by the fold: an arriving `User` entry drops the first content
match, the TUI's reconciliation rule (the submit response carries only a
status, so there is nothing better to key on; identical queued turns settle
one at a time, in order). Two orderings make delivery arrive outside the
fold, and both are handled: the entry's log frame can fold *before* the 202
body resolves (the daemon commits and broadcasts independently of writing the
response), so the push takes the pre-submit log cursor and skips itself when
a newer matching `User` entry already folded; and a re-seed (`seed`) settles
pending turns against the entries it absorbs beyond the cursor, since those
never pass through `applyLog`.

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

## The top bar

`ui/TopBar.tsx`: sticky interface chrome — the serif wordmark, the session
status dot (breathing while the loop runs), and the usage meter: context
occupancy as numbers and a thin bar, plus the session cost. Deliberately
minimal — the mock's bar is a dumping ground of imagined status items, and
additions happen one argued element at a time
([backlog/web/upstream-failure.md](../backlog/web/upstream-failure.md) is
the next argued one). Loop state is not restated here: the dot breathes,
and the banner, the gate cue, and the composer's turn-state carry the rest.
Two elements wait on the wire: the model name and the working directory,
which no `/v1` topic carries yet
([backlog/session-identity-topic.md](../backlog/session-identity-topic.md)).

## The composer area

The loop dials live on the composer's status row (`ui/Composer.tsx`): gates
arrive at the composer, so it is where current state is controlled.
Interrupt (`POST /v1/loop/interrupt`, enabled while `running`), hold/resume,
the pace select, and the review-level select — one `PATCH /v1/loop` for the
dials; the api layer's `patchLoop` translates to the wire's partial-`pace`
shape. The row leads with the turn state — a breathing accent dot beside
"your turn" in the serif when the loop is idle, a dim `running…` while it
moves — and the space between state and dials is reserved for the key-hint
strip (binds are deferred to interactive suggestions;
[backlog/web/README.md](../backlog/web/README.md)). While the loop runs, the
input carries a "rides along with the pending result" hint: the
mid-trajectory injection semantics ([control-channel.md](control-channel.md)),
stated at the point where the text is being typed.

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
that end a text selection, don't count) or its result header's `inspect →`
link, closed with `✕` — "raw is one tap away" promoted from the per-result
`<details>` to a standing surface. The
selection is a call id held in `App`, which builds the transcript tree once
and shares it with both consumers; the node is re-found from the tree each
render, so a result or verdict landing after the drawer opened shows up in
it. While the drawer is open the content column yields to it
(`body:has(.inspector)`) rather than being covered.

Tabs — `result` / `args` / `raw` / `spot`, all views over data the client
already holds, rendered through the cordon's existing renderers rather than
a second implementation; and `file`, present but inert — it is the punted
full-screen file view ([backlog/web/file-view.md](../backlog/web/file-view.md)),
kept visible so the shape is honest about what is coming. Below the tabs,
**what the fork returned**: the raw line count, the surfaced count computed
from the keep, the keep itself (`All` / `None` / `Ranges(a..b, …)`), the
wire state, and the distiller's note — the audit view of
[redaction-fork.md](redaction-fork.md)'s output. Stepping between calls from
the drawer waits on the deferred key binds.

With the inspector holding the whole body, **read and write results
truncate in the transcript**: the first 12 lines, then a
`⋯ N more lines — inspect →` affordance (never for a tail shorter than the
indicator itself). This is the first revision to render-whole, and it is
safe for the reason the position existed: a gate decides a *call*, never a
result, so no pending decision rests on a truncated body — and the full
body is one click away. Other tools' results still render whole.

## The gate surface

The pending approval gate (`web/src/ui/Gate.tsx`) renders from the `gate` state
topic, in the transcript flow above the composer rather than a modal, and is
**the affordance only**. The call it decides is already on screen: a gated call
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
away from. `GateCue` is a fixed marker, shown by an `IntersectionObserver` on the
surface exactly while the surface is out of view; it names the call (the part
that is off-screen when it shows) and jumps to the document tail. Two properties
are deliberate: it **never scrolls on its own** — a pending decision does not get
to move the page out from under someone reading — and the jump is a jump, not a
smooth `scrollIntoView`. `useFollowTail` pins to the bottom after every render
while the viewport is near it, so an animated scroll toward the tail re-engages
the follow partway down and is cancelled by the next render. Any future
scroll-to-somewhere affordance meets the same constraint.

All four decisions ride `PUT /v1/approvals/:call_id` (`Decision` in `api/types.ts`):
approve, abort, redirect with steering text, and allow-with-prefix (a
`config_edit` carrying a `command` change, prefilled from the call's own
`command` field, plus an optional note). A `409 gate_moved` is handled in
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
or a failed trade. The SPA index answers `/` only — no client-side routes exist yet,
and a broader index fallback would answer scanner probes `200`.

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
