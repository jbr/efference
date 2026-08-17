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
- **Tool results are plain text.** No port of the TUI's per-tool parsers; rich
  rendering waits for structured results / the daemon-side render document (the
  reserved `render` field on log frames). A UI finding made in either client is
  promoted to a shared home and inherited, never re-implemented per client.
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
