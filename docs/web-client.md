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
- **Content before styling.** The visual layer is deliberately minimal until the
  information surface is complete.

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
code bodies as the future syntax-highlighter's hook, and the raw content
behind a per-result `<details>` whenever the body isn't verbatim raw — the
raw/distilled toggle, from the entry the client already holds. Ports here
stay literal (same names, same strings, same classifications) and never grow
beyond mirrors: a rendering idea that isn't a mirror belongs to the
structured-output successor, not the cordon.

## Transcript ergonomics

Two behaviours over the render-whole transcript. **Follow-tail**
(`ui/useFollowTail.ts`): the transcript scrolls the window, so "following"
means the viewport sits at the document's bottom — new entries and streamed
output keep it pinned there, scrolling up holds position, and a cold open
lands at the tail rather than the top of the preamble. **The preamble
collapses** to a native `<details>` line that expands on tap — presentation
of one audit entry, not a fold affordance; render-whole for tool results
stands (the positions above).

## The controls strip

The operator dials and the ambient state (`ui/Controls.tsx`), rendered under
the header from the topics the store already carries: interrupt
(`POST /v1/loop/interrupt`, enabled while `running`), hold/resume and the
pace select (one `PATCH /v1/loop`, which also moves the review-level select —
the api layer's `patchLoop` translates to the wire's partial-`pace` shape),
and the usage line (context occupancy and the session cost, whichever halves
the provider filled). One row per process: command, elapsed (client-computed
from `spawned_at`, ticked once a second while any process runs), muted flag,
and the verbs — mute/unmute, the background handoff with its optional note
(`PATCH /v1/processes/:id`, a 202) on the foreground entry, kill
(`DELETE /v1/processes/:id`) on any. Pending notifications render one row
each with dismiss (`DELETE /v1/notifications/:id`) — on an idle loop the
inbox delivers immediately, so these rows only appear while a tool is
mid-run. A non-`live` connection shows as a full-width banner above the
strip, not just the header word.

## The gate surface

The pending approval gate (`web/src/ui/Gate.tsx`) renders from the `gate` state
topic, in the transcript flow above the composer rather than a modal — the call
whole (name, full input, the logged spotter stance + reason via the `spotFor`
selector), because a gate resolved over a snippet is not an informed choice. All
four decisions ride `PUT /v1/approvals/:call_id` (`Decision` in `api/types.ts`):
approve, abort, redirect with steering text, and allow-with-prefix (a
`config_edit` carrying a `command` change, prefilled from the call's own
`command` field, plus an optional note). A `409 gate_moved` is handled in
`session/actions.ts` by folding the response's corrective `detail` into the
store as a synthetic state frame — no second round-trip; the surface re-renders
from it. When the gate changes under the surface for any reason, the inputs
reset and a notice says so, because a decision composed against one call must
not land on another.

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
