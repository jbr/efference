# The host — one front door for every session on a machine

`efference host --bind <addr:port>` runs a process that is not a session: it binds one
authenticated listener and serves three things — the live-session listing, each
session's own control API under a `/sessions/<id>/` prefix, and the web client at the
root. It starts no sessions and owns none. Implemented in `src/host.rs` (the routes and
the liveness scan) and `src/host/proxy.rs` (the per-session forward); the listener, the
gate, and the credential arithmetic are the single-session bind's
([web-bind.md](web-bind.md), `src/daemon/tcp.rs` and `src/auth.rs`), reused rather than
re-implemented.

What exists today is the skeleton: read and attach. Credential consolidation, push
subscriptions, and session lifecycle (`POST /sessions`, stop, adopt) are later steps of
the same unit and are recorded in `backlog/host-process.md`.

## Why a process, and why a proxy

- **One process owns the network boundary.** Sessions are numerous and short-lived;
  giving each a reachable bind means each has its own cert, its own credential state,
  and its own exposure. Auth, TLS, and static assets are machine-scoped concerns with
  no per-session home.
- **A client holds one credential for one endpoint.** Authentication is per
  client↔host connection, never per session. This is why the host proxies rather than
  redirecting a client to the session's own bind: a redirect design would require a
  client to hold N credentials to watch N sessions.
- **The session speaks to exactly one peer.** Sessions behind the host stay UDS-only,
  so they need no auth model of their own. The single-session TCP bind
  ([web-bind.md](web-bind.md)) is unchanged and keeps working: it is the no-host escape
  hatch for one session, not a topology.
- **The API is identical across every binding** ([control-api.md](control-api.md)).
  The host adds no endpoint that translates or enriches the per-session API; its own
  surface is only what has no per-session home. `/sessions/<id>` is host routing, not a
  namespace inside the API — the "additive path prefix" branch of the migration intent
  control-api.md records. A client treats its control endpoint as an opaque base URL
  and joins paths onto it, which makes daemon-at-socket-root and host-at-prefix the
  same client code with different configuration.

Whether sessions stay separate processes or become tasks inside the host is open, and
deliberately invisible to clients; the skeleton forecloses neither.

## The listener

The bind and auth flags are `serve --bind`'s, with the same env fallbacks
(`EFFERENCE_BIND`, `EFFERENCE_HOST`, `EFFERENCE_TLS_CERT`, `EFFERENCE_TLS_KEY`) and the
same posture: plaintext only on a loopback address, TLS wants an explicit `--host`
name, a TLS bind also serves HTTP/3, and `EFFERENCE_ROOT_SECRET` must be present. Both
commands resolve them through one function, so they cannot disagree about what a
missing root or a routable plaintext address means. `--bind` is required here, since a
host with no listener has nothing to be; `--open` launches the operator's browser at
the printed bootstrap link.

The handler stack is the session bind's, in the same order — security headers, HEAD
rewriting, the cookie jar, the gate, the credential mint, the routes, the web client —
differing only in what sits between the gate and the client. The gate's one
listener-dependent decision is which prefixes are guarded, carried as a `Surface`
(`src/daemon/tcp.rs`): a session bind guards `/v1` and `/files`, the host guards
`/sessions`. Everything else — the Host allowlist, the Origin and Content-Type checks
on unsafe methods, cookie-or-bearer, the anonymous web-client shell — is the same code
answering the same way.

The credential is the machine's, so a cookie minted against a session's own bind
authenticates to the host and vice versa (cookies ignore ports, and the root is
machine-scoped). The host mounts the anonymous `POST /auth/session` mint; the
enrollment routes (`/auth/link`, `/auth/otpauth`, `/auth/bearer`) stay on the session
daemon's Unix socket, where possession of the host machine is the authority, so
`efference dev open` continues to address a session rather than the host.

## `GET /sessions` — the liveness scan, promoted

```jsonc
[ { "id": "work" }, { "id": "1754300000000" } ]
```

The array carries what the scan already yields and nothing else: the session dir's
name, which is the path segment a client joins its control base onto. A session's own
state is read through its own API at `/sessions/<id>/v1/state`, so the listing does not
duplicate it. Sorted by id, so two reads of an unchanged set are the same bytes.

**Liveness, not existence** — the same rule the CLI's socket discovery follows
([control-channel.md](control-channel.md)): each `<base>/<id>/control.sock` is dialed,
and a stale dir from a dead daemon refuses the connection and is left out. The base is
`.efference` under the host's working directory, so a host serves the sessions of the
project it was started in. A dead session's log stays on disk and readable by the
offline verbs; what this endpoint answers is which sessions can be talked to now.

A client showing a *list* also wants to know which sessions have a pending gate without
opening a stream per session. That is the cross-session summary stream, and it is not
built; `backlog/host-process.md` records why it is a coarser host-level resource rather
than a merge of the per-session streams. Until it exists the web client's list shows
the id and nothing else ([web-client.md](web-client.md)): the alternative is one
`GET /sessions/<id>/v1/state` per session on every load, which is the shape the summary
resource exists to replace.

## `/sessions/<id>/…` — the session's own API, forwarded

Everything after the prefix is proxied to that session's Unix socket byte-for-byte:
method, path, query, request body, response status, response body, and the session's
own error envelope on a failure it produced. `trillium-proxy` carries it, for two
properties a hand-rolled forward would have to reproduce:

- **The upstream response is a streaming body.** `GET /v1/events` flushes each frame
  as it arrives rather than buffering for the connection's lifetime, which for a stream
  that never ends would mean never answering at all.
- **A `101` is carried as an upgrade**, with both halves then copied full-duplex — so
  an upgrade-shaped transport (`backlog/ws-stream-encoding.md`) needs nothing here.

The router matches `/sessions/:id/*` and pushes the wildcard remainder onto the conn's
path, and the upstream selector builds the forwarded URL from *that* rather than from
the request's untrimmed path: forwarding the untrimmed path would send the session
`/sessions/<id>/v1/state`, which its router does not serve. The URL's authority is a
placeholder — the connector dials a fixed socket path and reads only the target and the
`Host` header.

One `Proxy` is built per session on first use and kept, so a session's requests share
one client and its connection pool instead of dialing a fresh socket each time.

**Ids are validated before they are joined onto a path.** An id must be a single plain
segment (ASCII alphanumerics, `-`, `_`, `.`, and neither `.` nor `..`); anything else
names no session. `serve --name` does not constrain the name today, so a session named
outside that set is not addressable through the host.

**An id that names no live session is `404 not_found`** — whether the dir is absent,
holds no socket, or holds a stale one, and including the case where the daemon exits
between the scan and the request (which reaches the proxy as an unreachable upstream).
Existence and liveness collapse into one answer on purpose: to a client holding a stale
id they are the same condition, and the recovery is the same re-read of `GET /sessions`.

## The web client at the root

The same embedded build the session bind serves ([web-client.md](web-client.md)),
mounted last so it answers exactly the GETs the routes do not claim. The client joins
every control path onto a base it reads off its own location — `""` at a session
daemon's root, `/sessions/<id>` here — so the host's root shows the session list and
`/sessions/<id>/` shows that session, from one build with no configuration.

**The client's own routes exist under a session prefix too.** The single-session view
loads *from* its base, so the shell has to answer there: `/sessions/<id>`,
`/sessions/<id>/`, and `/sessions/<id>/tree` are named in the host's router — claimed
from the proxy, which would otherwise forward them to a session whose router serves
the control API and not the client — and passed through to the frontend handler, which
answers them with the same index `/` and `/tree` get. Three predicates state the same
set and must move together: `Surface::spa_route` (`src/daemon/tcp.rs`, which is also
what makes the shell anonymous inside the otherwise-guarded `/sessions` prefix), these
routes, and the service worker's navigation fallback (`web/public/sw.js`). A path
under the prefix that names no client route is still the session's to answer.

The shell is served for any id the proxy would accept, live or not: whether a session
answers is `/sessions/<id>/v1/state`'s answer, and a client holding a stale id learns
the same way it learns anything else.

`/files` is mounted on a session daemon's *TCP* listener only, so a proxied
`/sessions/<id>/files/…` reaches the session's Unix socket, which does not serve it.
The file viewer and tree therefore refuse through the host today — as a refusal the
viewer renders, not a blank pane.
