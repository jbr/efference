# The TCP bind — the web client's front door

`efference --host --bind <addr:port>` adds a TCP listener beside the Unix socket. Both
serve the same `/v1` routes ([control-api.md](control-api.md)); the TCP listener wraps
them in a hardening + auth gate, while the UDS stays auth-exempt (filesystem permissions
are its trust model) and remains the CLI verbs' channel. Implemented in
`src/daemon/handler.rs` (the one stack every listener mounts), `src/daemon/tcp.rs`
(the listener, the gate, the mint and enrollment routes, the web client handler) and
`src/auth.rs` (the credential arithmetic); the flag resolution is
`src/bin/efference/serve.rs`.

This is the **one**-session bind. A multi-session host reuses everything in this
document — the flags, the listener shape, the credential layer, and the gate — rather
than shipping a second one. The gate's only listener-dependent decision is carried as
a `Surface`, and it is not a set of guarded prefixes: the anonymous surface is
enumerated, so every path outside it takes a credential whatever its name. What a
surface settles is where the paths that name a bind's shape rather than its content
sit — the web client's own routes, at a session bind's root and also under
`/sessions/<id>` on a host — and that `GET /sessions` on a session bind is the probe
that has to reach its router so its `404` says "not a host".
`Surface::Host` is the interface the in-process host mounts its session routes behind, so
the gate's decision is the same shape on both.

The bind is configured by the `[host]` table of the user configuration,
`~/.efference/config.toml` (`bind`, `public-url`, `tls-cert`, `tls-key`;
`src/inference/host.rs`), with a flag beside `efference --host` overriding each key alone —
`--bind ""` turns a configured bind off for one launch. The table describes the machine's
host rather than a shell, which is what lets a host started by a service manager, with no
interactive environment, carry the same bind. A leading `~/` in a certificate path
expands against the home directory. A terminal-owned island ignores the table: it takes
its address from `--bind` alone ([operating.md](operating.md)).

`--bind` is one flag with two homes, since both surfaces serve the same web client from
the same credential. A port (`--bind 8080`) is the loopback address with that port, an
address is read as written (`127.0.0.1:8000`, `0.0.0.0:0` for a port the kernel picks),
and a bare port can never widen exposure past the loopback rule below because it *is*
loopback. With `--host` it is the machine's front door; on any other invocation it
is the island's, and the island answers in the browser on that address rather than in the
TUI ([operating.md](operating.md)).

The bind is never set by a session's `config.toml`: that file is model-proposable and
fs-watched live, and network exposure must not be requestable from inside a session. The
user configuration is outside every session's grants — an edit a session proposes to it
is held for the operator like any write outside its perimeter.

## Listener topology is the trust model

The daemon runs two trillium servers sharing one `Swansong` (one shutdown domain) and
one `Arc<Session>`. Both mount the same stack, `handler(transport, app)` in
`src/daemon/handler.rs` — the only place a handler is assembled, for the daemon, the
TCP bind, the host, and every test. Its tuple is fixed; each layer that is not always
present is an `Option`, which trillium runs as a pass-through when `None`, and the
`Transport` picks which are `Some`:

- **Socket:** the security headers, HEAD rewriting, the `ETag` validator, the
  enrollment routes (below), the `/v1` routes. No gate, no web client.
- **Web:** the security headers, HEAD rewriting, the gate, the `ETag` validator, the
  credential mint, the `/v1` routes, `/files`, then the web client —
  every conn passes the gate first; the trailing frontend handler serves the
  embedded web client ([web-client.md](web-client.md)) for the GETs no route
  claims. The SPA
  index answers a **named list** of client routes (`SPA_ROUTES`: `/` and
  `/tree`) rather than every unmatched path, which would hand scanner probes a
  `200`; the service worker's offline navigation fallback mirrors that list, and
  new client routing is added there rather than by loosening the predicate. The
  list is relative to the client's base, so on a host the same routes are also
  answered under a `/sessions/<id>` prefix ([web-client.md](web-client.md)) and the
  predicate is a function of the listener's `Surface`; on this bind the base is
  empty and a `/sessions/…` path names nothing.

`head` is `trillium_head::head()`, mounted ahead of everything: HEAD requests are
rewritten to GET on the way in (so the gate and routers only ever see GET) and the
response body is stripped — content-length kept — on the way out.

The UDS exemption is therefore structural — which listener a conn arrived on — rather
than inferred per-conn (a `peer_ip().is_none()` style guess would fail open if a
transport stopped setting it).

**Response headers.** The outermost layer, `SecurityHeaders` (`src/daemon/tcp.rs`),
stamps every response each listener produces — route bodies, router errors, and the
web client alike — so a handler that answered without them would be the hole. It
carries the page-hardening headers ([web-client.md](web-client.md)) plus three
transport-side ones: `Cache-Control: no-store` on every `/v1` route, a
session-prefixed path counting as its inner route; `Cross-Origin-Opener-Policy:
same-origin`; and `Strict-Transport-Security: max-age=31536000` only when the
listener terminates TLS — the flag is the arm's, fixed at bind time, and there is no
subdomain scope or preload because the bind is one host. `/files` sets its own
`Cache-Control: private, no-cache` in its read stage, which keeps the header off the
route's change streams.

TLS terminates in-daemon: `--tls-cert`/`--tls-key` take PEM paths (the
`tailscale cert` output) into a `RustlsAcceptor`. A TLS bind also serves **HTTP/3**: a
QUIC listener on the same port (UDP, `trillium-quinn`), which the listener builder
auto-pairs with the TLS listener and advertises via `alt-svc: h3` — browsers upgrade
on a later request. A plaintext bind is accepted only on
a loopback address; the operator posture for anything routable is the tailnet address
with the ts.net cert. `public-url` (`--public-url`) is the address browsers open, an origin
(`https://name[:port]`, no path): the base of printed links, and by its host the Host
allowlist entry. It defaults to `http://localhost:<port>` on a plaintext loopback bind and
is required with TLS, where it must be `https`. It is given rather than derived because a
proxy terminating TLS in front of a loopback listener — `tailscale serve`, verified
2026-09-26 — puts browsers at an address the listener cannot infer: `https` on the proxy's
port. The proxy must forward the client's `Host` (serve does), and the Origin check compares
hosts only, so both hold behind it. The certificate and key are a pair,
checked after the table and flags are merged, since each may come from either.

## Credentials: one root, everything derived

One machine-scoped root secret lives in the secret store ([secrets.md](secrets.md))
as the `root` item: 32 bytes from the OS random source, minted the first time a
bind needs it and read at every bind after, held only in memory. Nobody supplies
it and nothing prints it. `src/auth.rs` derives everything else by HKDF-SHA256
with per-purpose context strings:

| derived | context | purpose |
| --- | --- | --- |
| TOTP secret | `totp v1` | the bootstrap credential (RFC 6238, SHA-1/6-digit/30 s) |
| bearer | `bearer v1` | the one TCP credential, `Authorization: Bearer` — the web client's and programs' |
| VAPID keypair | `vapid v1 attempt-<n>` | the web-push sender identity ([web-push.md](web-push.md)); the counter increments until the bytes are a valid P-256 scalar |

Same root in, same credentials out, every boot: enrollments and signed-in browsers
survive restarts, and revocation is root rotation — `efference auth rotate` deletes the item
and the next bind mints another, which signs out every browser, invalidates the
authenticator enrollment, and drops every push subscription (a mass logout — the
right granularity for a single-operator daemon). A store that answers a read with
an error is not treated as absent: minting over it would be an unasked rotation, so
the bind fails with the store's complaint and names the rotate verb. Received
secrets that cannot be re-derived (OAuth tokens) are out of scope here and stay with
[mcp-auth.md](mcp-auth.md). 32 bytes is the size the derivations can use: HKDF's
extract is 256-bit, the bearer and the key it is compared under are 256-bit, TOTP is
bounded by its six-digit codes and P-256 by its 128-bit level.

**One credential, held by the page.** Every request that costs a credential presents
the bearer as `Authorization: Bearer <token>`, the web client's included. The login
view trades a current code for it at `POST /auth/session` (below), and the client
keeps it in IndexedDB with an in-memory copy, read once at boot, that `apiFetch`
attaches to every request (`web/src/api/bearer.ts`, [web-client.md](web-client.md)).
IndexedDB is scoped by origin, and an origin includes its port, so no other service on
`localhost` reads it. The service worker can read it, and its push-subscription repair
authenticates with it ([web-push.md](web-push.md)). A `401` to the client's binding
probe or its session connection clears the stored copy, so the sign-in view starts
from none. The bearer carries no expiry: a browser stays signed in until the root
rotates or the browser drops the site's storage. Safari deletes script-writable
storage for a site not visited in seven days of browsing (apps added to the home
screen are exempt); whether that applies to a `localhost` or a `ts.net` origin is
unmeasured, and where it does, the browser signs in again.

The gate reads a credential from the `Authorization` header and nowhere else — no
cookie, no query parameter — and no answer sets a cookie. A request another site
makes the browser send — a navigation, a form post, an image — carries no header the
page did not set, so it carries nothing that authenticates. No answer grants another
origin CORS either: a page elsewhere, another port on `localhost` included, can
neither read an answer nor send the header, since the preflight a cross-origin
request with it needs is refused like any request without a credential.

The bearer is reachable from the page's script, which an `HttpOnly` cookie would not
be. The boundary for that is the page's Content-Security-Policy together with the rule
that nothing turns untrusted content into markup ([web-client.md](web-client.md)): a
first-party client with no third-party content, under a policy that names no other
origin. Script running in the page can make the page's requests and read their answers
whatever form the credential takes; what the bearer in reach adds is that such script
can carry it away.

A bind to port 0 learns its port once listening, and minted links name that port. A
link carries a code no one has used: the current step's, or when that was just redeemed
the next step's, which the one-step skew accepts; with both used, no link is minted
(`/auth/link` answers 429) rather than a dead one.

**The root is not in any environment.** It is read from the store into the binding
process and nowhere else, so no spawn site has to strip it. A tool child that wants
it must read the store itself, which on every backend is a visibly gated command
(`security`, or a keychain prompt against the signed binary); the gate is the
enforcement ([secrets.md](secrets.md)).

## The bootstrap dance

`--host --bind` prints `https://<host>:<port>/#token=<current code>`; `--open` also
launches the browser. `efference dev open` re-mints against a running daemon — the re-auth
path when at the host, and the answer to lost scrollback. It is a `dev` verb rather than a
`control` tool because it mints credentials for the HTTP surface, and `control` is the set
`efference mcp` exposes to whatever mounts it; the re-mint is expected to move into the TUI. The code rides the URL
*fragment*: it never reaches server or proxy logs, and burned codes make
browser-history residue inert. The web client's login view
([web-client.md](web-client.md), `web/src/ui/Login.tsx`) reads the hash, trades the
code at `POST /auth/session` for the bearer, keeps it (above), and scrubs the URL with
`history.replaceState`; a bare visit renders a six-digit field posting to the same
endpoint. `GET /auth/otpauth` on the Unix socket returns the `otpauth://` enrollment URI for a stock
authenticator app; nothing renders it as a QR any more (the CLI flag was removed pending a
TUI or web onboarding flow).

TOTP acceptance has two hard properties:

- **Accepted timesteps burn.** The `totp_burns` table of the user state database
  ([state-store.md](state-store.md)) is user-scoped — every daemon under the user derives
  the same TOTP secret, so they must share burns or a code accepted by one would replay
  against another. A step is the primary key, so the insert is the check: a conflicting
  one is a replay, decided inside sqlite rather than behind a lock. Rows are a *set* of
  recent steps (±1-step skew means codes arrive out of order, so a high-water counter
  would reject legitimate ones) and prune once they fall outside the window where a code
  could verify at all. The steps are counters, not secrets; losing them re-opens at most
  the current ~90 s window.
- **Verification is rate-limited with lockout.** Six digits is ~20 bits; the limiter
  is what makes that arithmetic acceptable. Five consecutive failures free, then a
  doubling lockout (30 s → 15 min cap), answered `429` with `Retry-After`. Per-daemon
  and in-memory: N daemons give an attacker N× the budget, accepted at tailnet
  posture.

## The gate

Runs before every TCP route, in order:

1. **Host allowlist** — the request's authority must be the configured name (plus
   loopback spellings on a dev bind). This is the DNS-rebinding check; `403
   forbidden`. The authority is the Host header on h1 and the `:authority`
   pseudo-header on h2 (what the TLS acceptor's ALPN negotiates with browsers); the
   gate reads both, since `trillium::Conn::host()` alone does not consult the
   pseudo-header (trillium-http 1.6).
2. **On unsafe methods:** a present `Origin` must resolve to an allowlisted host
   (`403`), and body-bearing methods (POST/PUT/PATCH) must declare
   `Content-Type: application/json`, or `application/octet-stream` for an attachment
   upload (`400`) — forms can't spell either, and a cross-site `fetch` carrying either is
   preflighted, which closes the no-preflight CSRF shapes. A localhost or LAN bind is not a trust boundary while a
   browser is present; these ship with the bind, not after it.
3. **Credential check** — the bearer in the `Authorization` header, else
   `401 unauthorized`. The rule is the **anonymous list, not the guarded one**:
   everything costs a
   credential except what is named as free, so a route added later is guarded
   unless it is named there too. The free surface is the web client's shell, its
   hashed assets, and the launch images under `/splash`, at its base
   ([web-client.md](web-client.md)), which reveal
   nothing session-specific; `POST /auth/session`, the mint, which the login
   flow cannot present a credential for; and on a session bind `GET /sessions`,
   which is no route of its — the web client's probe to tell the two bindings
   apart reads the `404` a session daemon gives it as "not a host"
   ([web-client.md](web-client.md)).
   Everything else takes the bearer, whatever its prefix: `/v1`, `/files`, the rest
   of a host's `/sessions` tree (`/sessions/<id>/files` included), and the host's
   own routes at its root. The `dev-proxy` build widens the free surface to the dev
   server's module URLs ([web-client.md](web-client.md)), because a shell that
   cannot load is a page with no login view on it.

The `401`/`403`/`429` codes are `ErrorCode` variants in the standard error envelope
([control-api.md](control-api.md)).

## `/files` — the operator's read surface over the working directory

`src/daemon/files.rs`, mounted on a web transport only, after the routes and before
the web client's frontend handler. It is a router over one `/files/*` route rather
than a handler that strips its own prefix, so the mount point is a route spelling
like every other and the path below it arrives as the router's wildcard. Every
answer is built from that capture alone — nothing reads the request path, and
nothing reads a trailing slash, which routefinder trims before matching
anyway. Read-only and human-facing: it is what running a
static file server beside the daemon was doing, moved inside it, so project files
are readable through the same page the session runs in — including from away from
the machine. It is the server half of the file viewer
([web-client.md](web-client.md)).

- `GET /files/<path>` → the file's bytes.
- `GET /files/<dir>` → a JSON listing: `path` (relative to the root),
  `recursive`, `truncated`, and `entries` of
  `{name, kind, size, modified_at, symlink}`, sorted by name. `kind` follows
  symlinks while `symlink` records that the entry is one.
- **A trailing slash names nothing.** Whether a path is a file or a directory is
  the filesystem's answer about the path, so `/files/sub` and `/files/sub/` are
  one resource served one way, and so are `/files` and `/files/`. There is no
  canonical spelling and no redirect to it: the listing carries its own `path`
  for a breadcrumb and the client composes absolute URLs, so nothing is resolved
  relative to the document URL, which is the only thing a trailing slash buys a
  static file server.
- `GET /files/<dir>?recursive=1&depth=N` → the same shape walked below the
  directory, with entry `name`s spelled as paths relative to it — the path
  filter's input ([web-client.md](web-client.md)). Both the depth and the entry
  count are capped in `files::list` rather than trusted; past either the walk
  stops and the listing says `truncated` rather than erroring, since a filter
  over most of the tree is useful and a partial set has to say so. Symlinked
  directories are listed but never descended: the walk would not terminate.
- Anything else, including any non-GET, is refused — this surface never mutates.

**Not under `/v1`, behind the same bearer.** `/v1` is the control API, where every
client is equal; `/files` is a human surface beside it, and the gate prices the two
alike. Working-directory reads from a bearer go past `[paths]` entirely, including the
injected deny that keeps a session out of every other session's log
([permissions.md](permissions.md)), so that fence holds while no session holds the
bearer. A session cannot derive it — the root is stripped from tool children — and
the web client keeps its copy in the browser's origin storage rather than anywhere a
session's environment carries. What remains is script injected into the web client,
which is the CSP's boundary (above) and which can make the page's requests and read
their answers whatever form the credential takes, and an operator pasting the bearer
into a turn, which no fence stops.

**Content-type is never the file's own.** Every body is
`text/plain; charset=utf-8`. Serving a repository's `.html` as `text/html` would be
same-origin script execution on a page that holds a session credential for a channel
that runs commands; `nosniff` rides every response already, so both halves are
covered.

**Revalidation, not caching.** The file answers set `Cache-Control: private,
no-cache` beside the `ETag`: a stored copy revalidates every use, which is what
keeps the `304` the viewer polls on correct across edits. The change streams
under the route set none.

**The root is the process working directory** — session-stable, the same root the
built-in tools resolve relative paths against — with no coupling to the session's
`[paths]` grants: the perimeter restrains the agent, not the operator. Resolution
canonicalizes and then re-checks containment, because the escape worth closing is a
symlink *inside* the root pointing out of it. (`..` is popped lexically too, though
over HTTP it never arrives — URL parsing removes dot segments before the request
goes out.)

**Nothing under the root is withheld, dotfiles included.** No path is refused for
being sensitive, and the listing's omissions above are volume rather than
concealment — `.git` and `target` stay readable by name. Concealing a path from the
listing while still serving it by name is the shape deliberately not built: it would
imply a protection that is not there. What binding therefore exposes, to
whoever holds the bearer, is every text file under the working directory —
`.env` among them. Two things frame that. The bearer is machine-scoped and lasts
until the root rotates, so the exposure window is the bearer's rather than the
session's; and the same bearer already reaches `/v1`, where approving a gate or
injecting a turn can have the
*agent* read any file into the transcript, so a stolen credential does not become
materially worse for this route existing. The widening that is real is narrower: an
operator who deliberately fenced the session into a subdirectory with `[paths]` will
still find the whole working directory readable through the browser, because that
fence is the agent's and this surface is not the agent's.

An operator with literal secrets in-tree is the case to know about, and `--bind` is
the opt-in that carries it. The alternatives were weighed and left unbuilt as
complexity ahead of need (jbr, 2026-08-20): a dotfile deny catches `.env` while
missing `secrets.yaml` and `*.pem`, and buys that partial cover at the cost of
`.github/`, `.envrc`, and `.claude/`; an operator-set exclusion would have to live in
argv or env rather than `config.toml`, since a knob widening what `/files` serves is
exactly the kind the session must not be able to propose.

**Refusals, not degraded renders.** A file that is not valid UTF-8 is
`415 unsupported_content`; one past `files::MAX_BYTES` (500 KiB) is
`413 too_large`. Both are stated so the viewer can say *why* rather than showing a
blank pane. This is also why a body is read whole rather than streamed: streaming
commits to `200` with its first byte, after which "not text" could only arrive as
replacement characters. Lifting the ceiling would mean range requests and
prefix-only validation together, which is a different design.

### The discovery ignore set

`files::ignores`, over the `ignore` crate: what git hides here is what discovery
hides. One set governs all three **discovery** surfaces — the flat listing, the
recursive listing, and the directory-scope change streams — while the read route
and single-file streams consult it not at all: **naming a path is the
permission**, so an ignored file stays readable, watchable and followable, and is
only absent from listings. Concealing a path from a listing while still serving
it by name would imply a protection that is not there; this is the same posture
stated above, now with git's answer instead of a hardcoded `target` /
`node_modules` list that is wrong for every project that is not this one.

Precedence is git's: the deepest `.gitignore` that matches decides, a `!`
whitelist at that depth wins, the root's `.git/info/exclude` and the user's
global ignore file participate, and `.git` itself is ignored unconditionally. A
`Scope` is the matcher chain from the root down to one directory, built per
request rather than cached, so an edited `.gitignore` takes effect on the next
listing.

The accepted consequence (jbr, 2026-08-21): the set is repository-aware, so
outside a repository — or for a file git cannot see — discovery shows nothing.
The known case is a `.git/info/exclude`-ed directory of planning documents; it
stays viewable through an `open ⤢` and undiscoverable in the tree. The
re-include knob when that bites is an efference.toml setting — which files a
project wants visible is a property of the project, not the human — and waits
until it does.

### Watching for changes

The same URLs content-negotiate: a `GET /files/<path>` whose `Accept` header
**explicitly names** `text/event-stream` gets a stream of change pokes instead of
the body. Explicit-only — `*/*`, a fetch's default, gets the file — so only a
client that asked to stream ever streams; the web client's reader is a `fetch`
that sets the header explicitly (`web/src/api/files.ts`). Behind the bearer like the
rest of the surface, and the subscription's lifetime *is* the connection: tab closes,
socket closes, watch drops. No registry, no control-channel frame class — the
control channel stays broadcast-topic shaped.

Behind the route sits the core watcher, `src/fswatch.rs`: one notify debouncer,
consumers on a tokio broadcast channel, watches registered per directory and
refcounted by guards. Events are **pokes, never content** — `event: change`,
data the file's root-relative path — and the client answers a poke with the same
conditional GET it would otherwise poll with, so no event is load-bearing: a
lagged receiver pokes rather than skips (the cost of a spurious poke is a 304;
the cost of a missed one would be correctness), and the channel stays shallow.

A file's watch is on its **parent directory** with a filter, for the same
reasons as the config watcher (`daemon/watch.rs`): notify cannot watch a missing
path, and editors save via rename-replace. The parent must resolve; the file
need not exist — so a stream opened against a refusal screen reports the file's
arrival, and one held across a delete reports the restore. The filter compares
against notify's reported spelling, so a path that resolves is adopted in
canonical form and its **canonical parent** is the watched directory — a
wrong-case URL on case-insensitive APFS, and a symlink resolving into a nested
directory of the root, would each otherwise open a stream that never fires
(review findings, 2026-08-21); a link out of the root is refused, as the read
route refuses it. After adoption a rename over the link *itself* is the edit
path that goes unseen — either spelling loses one of the two. Event data
percent-encodes CR/LF in names: a lone CR (legal on disk) reaching the wire
would let a workspace file name forge pokes, since browsers split SSE fields
on it. Streams idle-comment on the same 15 s heartbeat as `/v1/events`.

Two contract points from the same review. The client's half: neither
watch-then-subscribe order is airtight against a change landing during stream
setup, so the consumer must answer stream *open* — not just pokes — with a
conditional GET; the viewer does. A known unhandled edge: if the watched
*directory* itself is deleted and recreated, some notify backends kill the
watch silently and nothing here detects or re-registers it — the stream stays
open (heartbeating) but deaf. Accepted for now; the fix belongs to whatever
teaches the watcher about watch death, not to per-stream code.

A URL naming a **directory** streams that directory's own entries instead — the
tree holds one per expanded directory, so what is on screen updates itself
([web-client.md](web-client.md)). Non-recursive, like the file case: a
directory the tree has not expanded has no consumer for its contents' churn.
Events are filtered to direct children and through the discovery ignore set
above, so a build directory cannot flood a stream nobody could see the results
of; `is_dir` at poke time is best-effort, since a deleted entry cannot be
stat'd, and the cost of getting it wrong is one spurious poke. A lagged
receiver pokes with the directory itself, whose answer is the same re-list.
The stream route reads the filesystem the same way the read route does, so
either spelling of a directory — the root included — streams that directory.

## Auth routes live beside `/v1`, not under it

The first surface with no per-session home, previewing a host's routing split:
a host listener mounts the same mint.

On the TCP listener (anonymous): `POST /auth/session` — `{"code":"123456"}` →
`200` with `{"token":"<the bearer>"}` in the body, under `Cache-Control: no-store`
(a credential in a response is never kept for reuse); it sets no cookie.

On the UDS only (host possession is the enrollment authority): `GET /auth/link`
(a fresh fragment-token URL), `GET /auth/otpauth` (the enrollment URI), `GET
/auth/bearer` (the same bearer, for a program on the machine). The otpauth URI
carries the TOTP secret itself, which mints codes — and through the mint, the bearer —
at will; it is an enrollment, so it rides the socket only, never the TCP listener,
authenticated or not.

## Remote TUI

Settled without this bind: OpenSSH forwards Unix sockets
(`ssh -L local.sock:remote.sock`), so ssh is that auth and the headless case never
conflicts with the browser path.
