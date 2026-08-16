# The TCP bind — the web client's front door

`efference serve --bind <addr:port>` adds a TCP listener beside the Unix socket. Both
serve the same `/v1` router ([control-api.md](control-api.md)); the TCP listener wraps
it in a hardening + auth gate, while the UDS stays auth-exempt (filesystem permissions
are its trust model) and remains the CLI verbs' channel. Implemented in
`src/daemon/tcp.rs` (the gate, the routes, the spawn) and `src/auth.rs` (the
credential arithmetic); the flag resolution is `src/bin/efference/serve.rs`.

The bind is configured by CLI flags — each with an env fallback (`EFFERENCE_BIND`,
`EFFERENCE_HOST`, `EFFERENCE_TLS_CERT`, `EFFERENCE_TLS_KEY`) so a direnv'd shell can
carry a standing default, and `--bind ""` overrides such a default back to UDS-only —
but never by the session `config.toml`: the config file is model-proposable and
fs-watched live, and network exposure must not be requestable from inside the session.
The operator's shell env is the same trust domain as the operator's argv; the config
file is not.

## Listener topology is the trust model

The daemon runs two trillium servers sharing one `Swansong` (one shutdown domain) and
one `Arc<Shared>`:

- **UDS spawn:** `(head, host_router, router())` — no gate, plus the host-only
  credential routes (below).
- **TCP spawn:** `(head, gate, public_router, router(), frontend())` — every conn
  passes the gate first; the trailing frontend handler serves the embedded web client
  ([web-client.md](web-client.md)) for the GETs neither router claims.

`head` is `trillium_head::head()`, mounted ahead of everything: HEAD requests are
rewritten to GET on the way in (so the gate and routers only ever see GET) and the
response body is stripped — content-length kept — on the way out.

The UDS exemption is therefore structural — which listener a conn arrived on — rather
than inferred per-conn (a `peer_ip().is_none()` style guess would fail open if a
transport stopped setting it).

TLS terminates in-daemon: `--tls-cert`/`--tls-key` take PEM paths (the
`tailscale cert` output) into a `RustlsAcceptor`. A TLS bind also serves **HTTP/3**: a
QUIC listener on the same port (UDP, `trillium-quinn`), which the listener builder
auto-pairs with the TLS listener and advertises via `alt-svc: h3` — browsers upgrade
on a later request. A plaintext bind is accepted only on
a loopback address; the operator posture for anything routable is the tailnet address
with the ts.net cert. `--host` names the public DNS name — it is both the Host
allowlist entry and the hostname in printed links (defaults to `localhost` on a
plaintext loopback bind, required with TLS).

## Credentials: one root, everything derived

One machine-scoped root secret arrives in `EFFERENCE_ROOT_SECRET` at boot (the op
env-file pattern used for provider keys) and is held only in memory. `src/auth.rs`
derives everything else by HKDF-SHA256 with per-purpose context strings:

| derived | context | purpose |
| --- | --- | --- |
| cookie-HMAC key | `cookie-hmac v1` | signs the browser session cookie |
| TOTP secret | `totp v1` | the bootstrap credential (RFC 6238, SHA-1/6-digit/30 s) |
| bearer | `bearer v1` | `Authorization: Bearer` for programs |

Same root in, same credentials out, every boot: enrollments and cookies survive
restarts with nothing secret on disk, and revocation is root rotation (a mass logout —
the right granularity for a single-operator daemon). Received secrets that cannot be
re-derived (OAuth tokens) are out of scope here and stay with
[mcp-auth.md](mcp-auth.md).

Two credential kinds because `EventSource` cannot set an `Authorization` header — the
browser's ambient cookie is forced, not stylistic. The cookie
(`__Host-efference`) is HMAC-signed over its issued-at, `Secure; HttpOnly;
SameSite=Strict`, 30-day fixed expiry, and carries validity and nothing else — no user
id, no scopes, no server-side session store. Cookies ignore ports, so one daemon's
cookie authenticates to every daemon on the host: correct under the machine-scoped
key, which makes "authed" a property of the machine, not the port.

**The root never reaches tool children.** Both spawn sites (`src/bash.rs`,
`src/daemon/background.rs`) `env_remove` it: with the root, a child derives the bearer
and drives its own approval gates. This is hardcoded self-surface protection like the
session-dir path deny — deliberately not an `[env]`-style policy knob. (Same-user
processes can still read the daemon's environment via `ps`/`/proc`; the strip removes
the ambient path, so reaching the root requires a visibly suspicious gated command.
The gate is the enforcement; the strip closes the accident.)

## The bootstrap dance

`serve --bind` prints `https://<host>:<port>/#token=<current code>`; `--open` also
launches the browser. `efference open` re-mints against a running daemon — the re-auth
path when at the host, and the answer to lost scrollback. The code rides the URL
*fragment*: it never reaches server or proxy logs, and burned codes make
browser-history residue inert. The web client's login view
([web-client.md](web-client.md), `web/src/ui/Login.tsx`) reads the hash,
`POST /auth/session`, gets the cookie, scrubs the URL with `history.replaceState`; a
bare visit renders a six-digit field posting to the same endpoint. `efference open
--qr` renders the `otpauth://` QR so a stock authenticator app becomes a re-auth path
with no host access.

TOTP acceptance has two hard properties:

- **Accepted timesteps burn.** The ledger (`~/.efference/totp-burns`) is user-scoped —
  every daemon under the user derives the same TOTP secret, so they must share burns
  or a code accepted by one would replay against another. Writes run under an
  exclusive file lock; entries are a *set* of recent steps (±1-step skew means codes
  arrive out of order, so a high-water counter would reject legitimate ones) and prune
  once they fall outside the window where a code could verify at all. The steps are
  counters, not secrets; losing the file re-opens at most the current ~90 s window.
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
   `Content-Type: application/json` (`400`) — forms can't spell that, which closes the
   no-preflight CSRF shapes. A localhost or LAN bind is not a trust boundary while a
   browser is present; these ship with the bind, not after it.
3. **Credential check** — valid cookie or bearer, else `401 unauthorized`. The
   anonymous surface is every GET outside `/v1` — the web client's shell and hashed
   assets ([web-client.md](web-client.md)), which reveal nothing session-specific —
   plus `POST /auth/session` (the mint). `/v1` is the whole sensitive surface on this
   listener; a non-GET outside it fails closed rather than 404ing.

The `401`/`403`/`429` codes are `ErrorCode` variants in the standard error envelope
([control-api.md](control-api.md)).

## Auth routes live beside `/v1`, not under it

The first surface with no per-session home, previewing the host process's routing
split ([../backlog/host-process.md](../backlog/host-process.md) adopts the bind and
the root when it lands).

On the TCP listener (anonymous): `POST /auth/session` — `{"code":"123456"}` →
`Set-Cookie` on success.

On the UDS only (host possession is the enrollment authority): `GET /auth/link`
(a fresh fragment-token URL), `GET /auth/otpauth` (the enrollment URI), `GET
/auth/bearer` (the program token). The otpauth URI carries the TOTP secret itself — a
strictly stronger credential than a cookie — which is why it must never ride the TCP
listener, even authenticated: a stolen cookie could otherwise upgrade itself to
permanent access.

## Remote TUI

Settled without this bind: OpenSSH forwards Unix sockets
(`ssh -L local.sock:remote.sock`), so ssh is that auth and the headless case never
conflicts with the browser path.
