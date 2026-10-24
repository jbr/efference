# Remote access

## What you need to know

A host can serve the web client, so you can follow and drive sessions from a browser —
on this machine, or from a phone or another computer. It is off until you turn it on.
<!-- src: src/inference/host.rs::HostConfig; src/bin/efference/serve.rs::resolve_bind -->

- **On this machine only**, the host serves plain HTTP on a loopback address
  (`127.0.0.1`), which other devices cannot reach.
- **From other devices**, browsers must reach the host over HTTPS: either the host serves
  HTTPS itself, or a proxy such as `tailscale serve` does it in front of a loopback
  listener. efference refuses to start a plain-HTTP listener on anything but loopback,
  because anyone who could reach it could replay a browser's sign-in and drive the agent's
  tools.
- **Tailscale** is the arrangement this manual describes for other devices: it gives your
  devices a private network, and gives this machine a certificate they trust.
<!-- src: src/bin/efference/serve.rs::resolve_bind, loopback_addr, checked_public_url -->

Anyone who reaches the address and signs in can do anything you can do in a session,
including approving the agent's commands. So the question for any arrangement is who can
reach the sign-in page, and the answer should be: only your own devices.
<!-- src: src/daemon/tcp/gate.rs::gate, Surface::anonymous -->

### On this machine

Add a `[host]` table to `~/.efference/config.toml`:

```toml
[host]
bind = "127.0.0.1:7878"
```

Then start the host with `efference --host --open`. It prints a sign-in link and opens it in
your browser; without `--open`, open the printed link yourself. The link works once, and
only for about a minute.
<!-- src: src/inference/host.rs::HostConfig; src/bin/efference/serve.rs::resolve_bind (--open); src/daemon/host.rs::serve_host (logs the link); src/daemon/tcp.rs::mint_link; src/auth.rs::TOTP_STEP_SECS, TOTP_SKEW, BurnLedger -->
<!-- TODO: say how to get a fresh link from a host that has been running a while, once there is a non-dev way (today: `efference dev open`, a dev-feature verb). -->

The port is yours to choose; there is no default. `--bind` on the command line overrides the
table for one launch.
<!-- src: src/bin/efference/serve.rs::BindFlags::over, loopback_addr -->

With no host running, `efference` can also open in the browser instead of the terminal:
choose "In the browser" when setup offers it, or run `efference --bind 127.0.0.1:0`. The
terminal stays open, holding the session; quitting there ends it.
<!-- src: src/bin/efference/island/browser.rs::BIND, run, PROMPT; src/setup/handoff.rs; src/bin/efference/setup.rs::default_verb (bind reaches only a new standalone host) -->

### From your phone or another computer, with Tailscale

You need Tailscale installed and signed in on this machine and on the other device, with
MagicDNS and HTTPS certificates enabled for your tailnet (the DNS page of Tailscale's admin
console). Enabling HTTPS publishes this machine's tailnet name in public certificate logs.
<!-- src: Tailscale docs, "Enabling HTTPS" (kb/1153) -->

Your tailnet's access rules decide which devices can reach this machine; by default that
is every device in the tailnet. If the tailnet includes other people's devices, or this
machine is shared with another tailnet, restrict its port to your own devices in the
tailnet policy.
<!-- src: Tailscale docs, Serve (kb/1312: access rules apply to Serve) -->

Use `tailscale serve`, never `tailscale funnel`: Funnel publishes the address to the whole
internet, leaving only the sign-in between strangers and your agent's tools.
<!-- src: Tailscale docs, Funnel (kb/1223) -->

Then pick one of two arrangements. In both, find this machine's tailnet name in
`tailscale status --json`, as `Self.DNSName` without the trailing dot — a name like
`laptop.tail1234.ts.net`.

**Behind `tailscale serve`** — Tailscale manages the certificate. The host stays on
loopback and Tailscale forwards HTTPS from your tailnet to it.

```toml
[host]
bind = "127.0.0.1:7878"
public-url = "https://laptop.tail1234.ts.net"
```

```sh
tailscale serve --bg http://127.0.0.1:7878
efference --host
```

`public-url` is the address your other devices use. `tailscale serve status` shows what
Tailscale is serving.
<!-- src: src/bin/efference/serve.rs::checked_public_url; src/daemon/tcp/gate.rs::gate (Host check against public-url); tailscale serve --help (1.104.1) -->
<!-- TODO: how the serve setting persists across reboots, and the command that removes it (`tailscale serve --https=443 off` in assets/guide/host.md is unverified against 1.104; `reset` exists). Needs a live check. -->

**The host serves HTTPS itself** — needed for HTTP/3; you renew the certificate.

```sh
mkdir -p ~/.tls && chmod 700 ~/.tls
tailscale cert --cert-file ~/.tls/laptop.crt --key-file ~/.tls/laptop.key laptop.tail1234.ts.net
```

```toml
[host]
bind = "100.101.102.103:7878"                 # this machine's address: `tailscale ip -4`
public-url = "https://laptop.tail1234.ts.net:7878"
tls-cert = "~/.tls/laptop.crt"
tls-key = "~/.tls/laptop.key"
```

Bind to the Tailscale address, not to `0.0.0.0`, which would serve the sign-in page on
every network this machine is on. The host reads the certificate once, at start.
Tailscale's certificates expire; renew with `tailscale cert` again and restart the host.
<!-- src: src/inference/host.rs::HostConfig, expand_home; src/bin/efference/serve.rs::resolve_bind (read once; TLS accepts any address); src/daemon/tcp.rs::bind; tailscale cert --help (1.104.1) -->

Either way, `efference --host` prints an `https://…ts.net` sign-in link; open it on the other
device within a minute. If the page answers 403, `public-url` does not match the name in
the address bar.
<!-- src: src/daemon/host.rs::serve_host; src/daemon/tcp/gate.rs::gate -->

## How this works, precisely

### The `[host]` table

| Key | Flag beside `efference --host` | Meaning |
| --- | --- | --- |
| `bind` | `--bind` | `ADDR:PORT` to listen on. A bare port means `127.0.0.1`; port `0` lets the system choose. `--bind ""` turns the listener off for one launch. |
| `public-url` | `--public-url` | The origin browsers use, `https://name[:port]`, no path. The base of printed links, and the host name the listener answers to. Required with TLS; defaults to `http://localhost:<port>` without it. |
| `tls-cert`, `tls-key` | `--tls-cert`, `--tls-key` | PEM certificate chain and private key. Both or neither. In the table, a leading `~/` is your home directory. |
<!-- src: src/inference/host.rs::HostConfig, expand_home; src/bin/efference/cli.rs::HostFlags; src/bin/efference/serve.rs::resolve_bind, checked_public_url, loopback_addr, BindFlags::over -->

Each flag overrides its key for one launch. A standalone host ignores the table; only
`--bind` reaches it, and it serves plain HTTP on loopback. The host reads the table when it
starts, so a change takes effect at the next start.
<!-- src: src/bin/efference/serve.rs::host (island branch), resolve_bind; src/bin/efference/island.rs::Island::launch -->

### Protocols

With `tls-cert` and `tls-key`, the listener serves HTTP/2 and HTTP/1.1 over TLS, and
HTTP/3 over QUIC on the same port number (UDP), which browsers switch to after their
first request. HTTP/3 needs that UDP port reachable. Without TLS, the listener serves
HTTP/1.1, and only on loopback: an address that resolves to anything else stops the host
at launch with an error that says why. Behind `tailscale serve`, Tailscale terminates TLS
and efference's listener is plain HTTP on loopback.
<!-- src: src/daemon/tcp.rs::bind; src/bin/efference/serve.rs::resolve_bind -->

### Request checks

Every request's host name must match `public-url`'s; a plain-HTTP listener also answers to
`localhost`, `127.0.0.1`, and `[::1]`. A TLS listener reached by IP address or by another
name answers 403. These checks stop a web page from tricking your browser into reaching
the host under another name; they are not a password, since anyone can send the right name.
A state-changing request from a page on another site is refused, and `POST`, `PUT`, and
`PATCH` must declare a JSON or binary body. Every response tells the browser to load
nothing from other origins and not to be framed.
<!-- src: src/daemon/tcp/gate.rs::gate; src/daemon/tcp.rs::TcpAuth::host_allowed, host_only, origin_host; src/daemon/tcp/headers.rs::CSP, SecurityHeaders -->

### Sign-in

The printed link works once, within about a minute, and signs one browser in, with no
expiry of its own. Other programs use the same token a signed-in browser holds, available
from the host's Unix socket. The link and the token both derive from one secret the host
keeps in your OS keychain, created the first time a host listens on TCP.
<!-- src: src/daemon/tcp.rs::mint_link, authenticate_code, TcpAuth::authenticated; src/daemon/tcp/routes.rs::mint_session; src/auth.rs::ROOT_ITEM, Root::load, Root::bearer -->

`efference auth rotate` replaces that secret, which ends every browser's sign-in when the
host next starts. `rotate` refuses while a local daemon is listening — it would keep
serving the root the store no longer has — so after a lost device: stop the host, rotate,
start it again.
<!-- src: src/auth.rs::Root::rotate (deletes the item only); src/bin/efference/main.rs::local_daemon_is_listening (the refusal); src/daemon/tcp.rs::arm_with_ledger (keys derived once at start) -->

The browser keeps its sign-in where only pages at the host's own address can read it:
other sites cannot, and neither can other servers you run on this machine at other
ports.
<!-- src: web/src/api/bearer.ts::BEARER_DB, bearer; web/src/api/http.ts::apiFetch; src/daemon/tcp.rs::TcpAuth::authenticated -->
<!-- TODO: the sign-in model (enrollment, per-device revocation, step-up) is being reworked in backlog/inbox/browser-sign-in-by-approval.md; this section stays at this level until it lands. -->

Other accounts on this machine can connect to a loopback port too. They still need to sign
in, but do not give a host a web address on a machine you share with people you do not
trust.
<!-- src: src/bin/efference/serve.rs::resolve_bind (loopback is per machine, not per user) -->
