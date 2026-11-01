# Remote access

You can check on a running session from your phone, answer a permission prompt, or
continue a conversation from another computer. The web client connects to the host on
your machine; browser access stays off until you turn it on.
<!-- src: src/config/user_config/host.rs::HostConfig; src/bin/efference/serve.rs::resolve_bind -->

## Choose where you’ll connect from

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

Treat a signed-in browser like an open terminal to your sessions: it can do everything
you can, including approve the agent’s commands. Keep the sign-in page reachable only
from your own devices, and approve sign-ins only for browsers you’re using.
<!-- src: src/daemon/tcp/gate.rs::gate, Surface::anonymous -->

## Open the web client on this machine

Add a `[host]` table to `~/.efference/config.toml`:

```toml
[host]
bind = "127.0.0.1:7878"
```

Then start the host with `efference --host --open`. It prints a sign-in link and opens it in
your browser; without `--open`, open the printed link yourself. The link works once, and
only for about a minute and a half. A host that has been running a while needs no new
link: open its address, and approve the code the page shows ([Sign-in](#sign-in)).
<!-- src: src/config/user_config/host.rs::HostConfig; src/bin/efference/serve.rs::resolve_bind (--open); src/daemon/host.rs::serve_host (logs the link); src/daemon/tcp.rs::TcpAuth::mint_link; src/platform/sign_ins.rs::Store::approved_link, TTL_SECS; web/src/ui/Login.tsx -->

Use a different port if you prefer; there is no default. You can also pass `--bind` on the
command line to override the table for one launch.
<!-- src: src/bin/efference/serve.rs::BindFlags::over, loopback_addr -->

With no host running, `efference` can also open in the browser instead of the terminal:
choose "In the browser" when setup offers it, or run `efference --bind 127.0.0.1:0`. The
terminal stays open, holding the session; quitting there ends it.
<!-- src: src/bin/efference/island/browser.rs::BIND, run, PROMPT; src/setup/handoff.rs; src/bin/efference/setup.rs::default_verb (bind reaches only a new standalone host) -->

## Connect from your phone with Tailscale

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

There are two ways to connect the host. Use `tailscale serve` if you want Tailscale to
manage the certificate. Have efference serve HTTPS itself if you want HTTP/3 and are
prepared to renew the certificate.

For either option, find this machine’s tailnet name in `tailscale status --json`, under
`Self.DNSName`. Remove the trailing dot; the result looks like `laptop.tail1234.ts.net`.

### Let Tailscale manage HTTPS

The host listens on loopback while `tailscale serve` forwards HTTPS from your tailnet to
it. Put this in `~/.efference/config.toml`, replacing the example name with your own:

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

### Have efference serve HTTPS directly

For HTTP/3, give efference a certificate and let it handle HTTPS itself. You’ll need to
renew that certificate. Start by requesting one for your machine’s tailnet name:

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
<!-- src: src/config/user_config/host.rs::HostConfig, expand_home; src/bin/efference/serve.rs::resolve_bind (read once; TLS accepts any address); src/daemon/tcp.rs::bind; tailscale cert --help (1.104.1) -->

### Sign in on the other device

Either way, `efference --host` prints an `https://…ts.net` sign-in link; open it on the other
device within a minute and a half, or open the address there and approve the code the page
shows. If the page answers 403, `public-url` does not match the name in the address bar.
<!-- src: src/daemon/host.rs::serve_host; src/platform/sign_ins.rs::TTL_SECS; src/daemon/tcp/gate.rs::gate -->

## Remote access details

### The `[host]` table

| Key | Flag beside `efference --host` | Meaning |
| --- | --- | --- |
| `bind` | `--bind` | `ADDR:PORT` to listen on. A bare port means `127.0.0.1`; port `0` lets the system choose. `--bind ""` turns the listener off for one launch. |
| `public-url` | `--public-url` | The origin browsers use, `https://name[:port]`, no path. The base of printed links, and the host name the listener answers to. Required with TLS; defaults to `http://localhost:<port>` without it. |
| `tls-cert`, `tls-key` | `--tls-cert`, `--tls-key` | PEM certificate chain and private key. Both or neither. In the table, a leading `~/` is your home directory. |
<!-- src: src/config/user_config/host.rs::HostConfig, expand_home; src/bin/efference/cli.rs::HostFlags; src/bin/efference/serve.rs::resolve_bind, checked_public_url, loopback_addr, BindFlags::over -->

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

A browser signs in with a link or with a code.

The link `efference --host` prints works once, within about a minute and a half, and signs
one browser in. A standalone host serving the browser opens one for you.
<!-- src: src/daemon/tcp.rs::TcpAuth::mint_link; src/platform/sign_ins.rs::Store::approved_link, Store::claim, TTL_SECS; src/daemon/host.rs::serve_host (logs the link); src/bin/efference/island/browser.rs::Links::mint; web/src/ui/bootClaim.ts -->

If you open the host’s address without a link, the browser shows a short code such as
`KQ7-M4X`. Within a minute and a half, run this on the host machine:

```sh
efference devices approve KQ7-M4X
```

You can also run `efference devices approve` and type the code when asked. Case and the
dash don’t matter. The command prints the browser’s name and how long ago it asked; the
browser signs in within a second. Expired or used codes won’t work, and the page offers a
new one.
<!-- src: web/src/ui/Login.tsx::Login, POLL_MS; src/daemon/tcp/routes.rs::request_sign_in, claim_sign_in, approve_sign_in; src/platform/sign_ins.rs::ShortCode::parse, Store::approve, TTL_SECS; src/bin/efference/devices.rs::DevicesCommand::Approve, ask_for_code -->

Away from the host, ask an agent session to run `efference devices approve` with the code
your new browser shows: the permission prompt shows the command with the code in it, and
approving the prompt is your check that it is the right code. Approve only a code you read
off a browser you are signing in. `efference devices` never shows a waiting browser's code,
only its name and how long ago it asked, so nobody can approve one by picking it from a
list.
<!-- src: src/bin/efference/devices.rs::listing; src/daemon/tcp/routes.rs::list_pending; src/platform/sign_ins.rs::Pending, Store::waiting -->

### Manage signed-in devices

Each browser that signs in becomes an enrolled device with a token of its own, with no
expiry. `efference devices` lists them: an id, the kind, how each joined (`link`, `code`, or
`add` for a program), when, when it was last seen (recorded at most hourly), and a label
naming the browser, such as "Safari on macOS". It asks the running host, so the host has to
be up with its web address on.
<!-- src: src/daemon/tcp.rs::TcpAuth::enroll; src/daemon/tcp/routes.rs::list_devices, with_auth (no bind armed); src/platform/devices.rs::Store, Joined, SEEN_INTERVAL_SECS; web/src/ui/deviceLabel.ts::deviceLabel; src/bin/efference/devices.rs::run, listing -->

`efference devices revoke <id>` signs one device out: its token is refused from its next
request on. The id is the one `efference devices` lists, or its first few characters. A
browser signed out this way signs in again, with a fresh link or a code, as a new device.
<!-- src: src/bin/efference/devices.rs::matching; src/daemon/tcp/routes.rs::remove_device; src/platform/devices.rs::Store::remove, Store::seen; src/daemon/tcp.rs::TcpAuth::credential -->

A program that reaches the host over the network gets a token of its own from
`efference devices add <label>`, which prints it once and never again. The program sends
it as `Authorization: Bearer <token>`, and `efference devices revoke` ends it like any
other device.
<!-- src: src/bin/efference/devices.rs::DevicesCommand::Add; src/daemon/tcp/routes.rs::add_device; src/daemon/tcp.rs::TcpAuth::credential -->

### Sign every device out at once

Every token derives from one secret the host keeps in your OS keychain, created the first
time a host listens on TCP; neither the list of devices nor the list of waiting sign-ins
holds a secret. With secret commands in place of the keychain, `efference auth rotate`
creates it instead, before the first host listens ([Credentials](credentials.md)).
`efference auth rotate` replaces that secret and deletes every enrolled device and waiting
sign-in, which signs out every browser and every program at once.
`rotate` refuses while a local daemon is listening — it would keep serving the root the
store no longer has — so stop the host, rotate, and start it again. One lost device needs
only `efference devices revoke`.
<!-- src: src/platform/auth.rs::ROOT_ITEM, Root::load, Root::rotate, DeviceTokens; src/platform/sign_ins.rs::SCHEMA (claim digests), Store::clear; src/bin/efference/main.rs::rotate, local_daemon_is_listening (the refusal); src/daemon/tcp.rs::arm_with_state (keys derived once at start) -->

### Where the browser keeps its sign-in

The browser keeps its sign-in where only pages at the host's own address can read it:
other sites cannot, and neither can other servers you run on this machine at other
ports.
<!-- src: web/src/api/bearer.ts::BEARER_DB, bearer; web/src/api/http.ts::apiFetch; src/daemon/tcp.rs::TcpAuth::credential -->

Other accounts on this machine can connect to a loopback port too. They still need to sign
in, but do not give a host a web address on a machine you share with people you do not
trust.
<!-- src: src/bin/efference/serve.rs::resolve_bind (loopback is per machine, not per user) -->
