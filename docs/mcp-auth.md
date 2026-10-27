# MCP authorization — the OAuth dance for connected HTTP providers

A connected provider reached over streamable HTTP (docs/tools.md) may answer `401`
instead of a tool table. The MCP authorization spec's answer is OAuth 2.1 with a
selected subset of standards-track features; this doc describes how that subset is
laid out here, and which invariants the layout exists to protect.

Stdio providers are out of scope by the spec's own direction: they take credentials
from the environment, not from this flow.

## Three layers, drawn as three interfaces

The dance is almost entirely generic OAuth. Keeping that separated from the MCP
parts is deliberate — each layer has a different eventual home, and mixing them
would strand reusable code inside a harness.

| Layer | Module | Knows about |
| --- | --- | --- |
| `WWW-Authenticate` challenge parsing (RFC 9110 §11.6.1) | `src/www_authenticate.rs` | HTTP only — no OAuth, no MCP |
| OAuth 2.1 public client | `src/oauth.rs` (+ `src/oauth/`) | OAuth RFCs only — no MCP |
| MCP glue | `src/mcp/auth.rs` | when the dance runs, and what the resource indicator points at |

The bottom two are written to be lifted out: the challenge parser wants a standalone
crate alongside `trillium-basic-auth` (which hand-rolls the emit half of the same
grammar), and the OAuth layer wants a `trillium-oauth-client`. Neither has moved yet,
because the API shape is worth deciding with a real consumer in hand rather than
before one. Keep MCP types out of both.

## efference is a public client

A shipped binary can hold no secret: any client secret compiled into it would be in
every copy on every machine. So `token_endpoint_auth_method` is `none`, there is no
client authentication at the token endpoint, and **PKCE carries the entire
per-authorization security burden.**

That is why `src/oauth/pkce.rs` offers `S256` and nothing else, and why
`src/oauth/metadata.rs` *refuses* an authorization server that does not advertise
`code_challenge_methods_supported` rather than proceeding without PKCE. There is no
second line of defense behind it to fall back on.

## Discovery has ladders, and they are ordered

Both metadata documents have more than one place they might live, and the order is
normative rather than a matter of taste (`src/oauth/metadata.rs`):

- **Protected resource** (RFC 9728): the `resource_metadata` parameter from the
  `WWW-Authenticate` challenge wins if present; otherwise the well-known URI is
  probed with the server's own path inserted (`/.well-known/oauth-protected-resource/public/mcp`),
  then at the origin root.
- **Authorization server**: RFC 8414 and OpenID Connect Discovery place a
  path-carrying issuer's path on opposite sides of the well-known segment, and
  deployed servers do both, so an issuer with a path gets three spellings and one
  without gets two.

A rung that does not answer `2xx` with parseable JSON is a rung, not an error. Only
running out of rungs is an error.

Two checks happen at fetch time rather than being left to callers, because
forgetting either is silent:

- **Issuer identity.** An authorization-server document's `issuer` must be
  byte-identical to the issuer identifier its URL was built from (RFC 8414 §3.3) —
  no case folding, no trailing-slash normalization. Without it, a document served
  from an attacker's origin could claim to be an honest issuer and redirect the
  whole dance.
- **PKCE support**, as above.

The resource identifier that the RFC 8707 `resource` parameter carries is the
`resource` field from the protected-resource document — *not* the URL efference
happened to connect to. That field is what names the audience the authorization
server will mint a token for.

## Credentials are per-authorization-server, never portable

Client identifiers are issued by, and unique to, one authorization server. Anything
persisted from dynamic registration is keyed by the issuer that issued it, and is
never replayed against a different one — if a protected resource starts naming a
different authorization server, that is a re-registration, not a reuse.

Client ID Metadata Documents are the exception, and the reason they are the spec's
preferred direction: the `client_id` *is* a self-hosted HTTPS URL the authorization
server resolves on demand, so it is portable across servers with nothing stored.

## The loopback redirect uses a fixed port

The authorization response comes back to a listener on `127.0.0.1`. The port is
**fixed, not ephemeral** — MCP requires authorization servers to validate redirect
URIs by exact match, which overrides the usual RFC 8252 §7.3 habit of letting a
native client's loopback port vary. A fixed port is what a static Client ID Metadata
Document can name, and what a stored dynamic registration stays valid for.

The port ladder is 50505, 50506, 50507 (IANA dynamic range), so an occupied port is
not fatal. All three are listed as `redirect_uris` wherever redirect URIs are
declared; both the `127.0.0.1` and `localhost` spellings are offered, because
authorization servers differ on which they accept.

## The client identity

efference's `client_id` for the metadata-document mechanism is
`https://efference.net/oauth/client-v1.json`, and `client_name` is `efference`. It
identifies the *software*, not an installation — every efference sends the same
value, and PKCE plus the loopback redirect are what make one authorization distinct
from another.

The path is versioned because the URL is a permanent identity: the document is
fetched fresh on each authorization, so editing it changes behavior for every
already-shipped binary pointing at it. A change that cannot be made compatibly needs
a new path.

`ClientMetadata` renders both the dynamic-registration request body and the hosted
document from one struct, so the redirect URIs an authorization server validates
against cannot drift from the ones the binary listens on. A test in `src/mcp/auth.rs`
pins the rendered document against what is hosted; that test failing means the hosted
file needs updating, not the test.

## Who decides what

`src/mcp/auth.rs` runs the sequence but does not decide how to reach a human: it
hands the authorization URL to a `present` callback and waits. A terminal can open a
browser; a log-captured boot can only print a URL. Keeping that at the call site is
what makes the sequence testable.

## When the dance runs

At boot, inside the mount loop (`src/boot/mcp_mount.rs`), which is the only
place the harness stops and waits for a human mid-startup. Per server:

1. Mount with whatever credentials are stored, refreshing a stale grant first — one
   round trip beats a `401` plus full rediscovery.
2. If the mount returns `AuthorizationRequired` (a typed error, not a message —
   the boot step has to recognize it among every other way a mount fails, and
   read the challenge out of it), run the dance and mount again with the token.
3. Anything else is a mount failure like any other: fatal unless `optional = true`.

How a mount meets step 2 is the preparation's `AuthorizationHandling`. On a host the
session is *starting* while it mounts ([control-api.md](control-api.md), "A starting
session"): the dance names the server it waits on in the session's `starting` topic
(`authorizing`) as it begins, and clears it as it ends, so the wait shows in every
client, and archiving the starting session drops the wait. A preparation that
`refuse`s authorization — a creation or resume sent with `refuse_authorization`, for
when nobody may be at the host — never starts the dance: the server's mount fails
saying it asks for authorization, which is a mount failure like step 3.

`efference dev spotter --tool` mounts through the plain path with no credentials and no
dance, so a spotter check can never open a browser; a server needing authorization
fails there with a message pointing at a hosted session. `--reauth <server>` discards stored
credentials first, for a revoked grant or a changed authorization server.

The authorization URL is always printed *and* the browser is always attempted. The
browser is not gated on a tty: a tty check looks like the right test and is not,
because a wrapper that filters the child's output makes stdout and stderr pipes
without making the session less interactive. `op run`, which masks secrets in what
it wraps, does exactly that — the first real authorization printed "no terminal
attached" to a human at a keyboard and declined to open their browser.

Attempting always is safe because the URL is printed either way, and the failure
modes are not symmetric: a spurious open attempt costs a line of output, a spurious
refusal leaves someone staring at a stalled boot. The bare URL gets its own line,
with the OSC 8 hyperlink layered on top rather than instead — captured output is
exactly where the escape is least likely to be honored.

## Renewal while a session runs

A mount hands the HTTP transport (`src/mcp/http.rs`) the server's stored credentials
(`auth::Credentials`, `src/mcp/auth/credentials.rs`) rather than a bare header, and the
transport renews them through the same refresh the mount runs on a stale grant
(`oauth::token::refresh`): before a request when the grant is past its expiry, and when
the server answers `401`, after which the request is sent once more with the renewed
grant. No person is involved while the refresh token works.

A renewal holds a lock for the server (`store::RefreshLocks`) from its load to its save.
It re-reads the stored grant and takes it without refreshing when it differs from the
grant the transport holds and has not expired: another session's renewal, or a new
authorization, has already replaced it. Otherwise it refreshes the stored grant, whose
refresh token is under rotation the only one the issuer still honours, and writes the
result back to `mcp:<server>` ([secrets.md](secrets.md)). Every session on a host mounts
with a store handle sharing the host's locks (`Host::mcp_refresh_locks`), so concurrent
calls across the host's sessions share one refresh, and a rotated refresh token is not
presented twice. Separate processes (two hosts, `efference check`) share the item but not
the locks; the re-read narrows that race without closing it.

A renewal that fails (nothing stored, no refresh token, a refresh token the issuer
refuses, an unreachable issuer), or a renewed grant the server refuses as well, ends the
request as `AuthorizationRequired`, carrying why. A call fails with it as its result,
which names the server and says the sign-in expired and a session restart signs in again:
the restart's mount refreshes once more and runs the dance if that fails. One request is
renewed at most once, so a server that refuses every grant costs a call one refresh. At
mount the same error is a `401` like any other and starts the dance. A transport mounted
without credentials renews nothing.

## Credential storage

`src/mcp/auth/store.rs` keeps one item per server in the secret store
([secrets.md](secrets.md)), named `mcp:<server>` and holding the `StoredAuth`
record as JSON. `TokenStore` is the interface the flow is written against;
`KeychainStore` is its one implementation. The item is per user: an authorization
belongs to the operator and the server, so a second checkout of a project reuses
the grant. A record that does not parse is a hard error, not "no credentials" —
the latter would quietly restart the browser dance and overwrite it.

Two things are stored, and only one is obvious. The **token** saves a browser
round-trip per launch. The **registration** saves the authorization server from
accumulating a new anonymous client record per launch, because dynamic registration
is not idempotent. A Client ID Metadata Document has nothing to store, which is one
of its quieter advantages.

## What is not built

Signing in again mid-session (a renewal that fails fails the call until the session
restarts; `backlog/inbox/remote-mcp-authorization.md`), `403 insufficient_scope` step-up,
pre-registered client credentials in config, and the subagent case. See
`backlog/inbox/mcp-auth.md`.
