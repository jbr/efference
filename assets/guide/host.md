# The host, the user-wide config, and the web interface

What sits above any one project: where efference keeps its connections to model
providers, the process that holds sessions, and how to reach sessions from a browser or a
phone. `efference://guide/new-project` is the per-project setup.

Everything here lives in the user's home directory, outside every session's grants. You
can explain it and propose edits; a write you propose there is held for the operator to
approve, and the choice to expose anything over a network is theirs.

## The user-wide config: `~/.efference/config.toml`

`efference setup` writes it on first run and edits it on later runs. It holds no secrets —
only where each credential comes from — and it is read when a host starts.

```toml ~/.efference/config.toml
[defaults]
model = "anthropic/claude-sonnet-4-5"   # <connection>/<model the provider knows>
spotter = "anthropic/claude-haiku-4-5"  # optional
triage = "anthropic/claude-haiku-4-5"   # optional

[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }  # or { kind = "keychain" }
```

- **Connections** name a provider and where its credential comes from: an environment
  variable (`kind = "env"`), the OS keychain (`kind = "keychain"`, which `efference setup`
  fills when a key is pasted), or for a ChatGPT subscription, Codex's login or one made in
  setup. `disabled = true` keeps one written down without using it.
- **Three roles** run models in a session. The **main model** is the agent itself. The
  **spotter** reviews tool calls the project's grants do not clear. **Triage** reads each
  tool result and keeps the lines that matter.
- **What each role falls back to:** the main model has no fallback — sessions need
  `[defaults].model` or a model named when they start. An unset spotter uses the main
  connection's `spotter-model` if it has one, then jev — a model built for that decision —
  when a TypeSafe or OpenRouter connection exists, then the provider's cheap model. An
  unset triage shares the main model.

To change a connection or a key, the operator runs `efference setup`: it checks
credentials live and keeps pasted keys out of the file. A different default model is a
one-line edit to `[defaults]` that you can propose.

## The host

A **host** is the process that holds sessions and runs them. The terminal UI, the web
interface, scripts, and other agents are all clients of it.

- **A plain `efference`** with no host running starts one for that terminal — an
  *island* — and ends it, with its sessions, when the terminal UI exits.
- **`efference host`** starts a host that stays up until stopped, at
  `~/.efference/host.sock`, where every project's `efference` finds it. Sessions then
  outlive any window showing them, several projects share it, and it is the host the web
  interface is served from. It reads its model credentials when it starts, so a key in an
  environment variable must be set in the environment it starts from.

## The web interface

The host can also listen over HTTP, serving a browser client for the same sessions. The
`[host]` table in the user-wide config says where; a flag on `efference host` overrides
each key for one launch.

On this machine only, over loopback:

```toml ~/.efference/config.toml
[host]
bind = "127.0.0.1:7878"
```

`efference host` then prints a link carrying a one-time code; opening it signs the browser
in (`efference host --open` opens it). A loopback bind is plain HTTP, which browsers treat
as secure for `localhost`.

## From a phone or another computer: Tailscale

Reaching the host from another device needs two things: a network path to it, and HTTPS
with a certificate the device trusts — the web client's sign-in cookie, offline support,
and notifications all require a secure origin. Tailscale provides both: a private network
between the operator's devices, and `tailscale cert`, which issues a trusted certificate
for this machine's name on that network. It is the one arrangement this guide describes.
Anything else — another VPN, a public tunnel, a home-made certificate authority — is the
operator's own design, with its own security trade-offs; do not propose one.

There are two ways to arrange it. Both need Tailscale installed and signed in on this
machine and the other device, and HTTPS certificates enabled for the tailnet in its admin
console.

**Bind to the Tailscale address.** The host serves HTTPS itself, with a certificate from
`tailscale cert`.

1. Find this machine's Tailscale address and name: `tailscale ip -4` prints the address;
   the name is the `DNSName` in `tailscale status --json`, without its trailing dot — a
   name like `laptop.tail1234.ts.net`.
2. Get a certificate: `tailscale cert laptop.tail1234.ts.net` writes a `.crt` and a `.key`
   into the current directory. Keep them together somewhere private, such as `~/.tls/`.
3. Point `[host]` at them:

```toml ~/.efference/config.toml
[host]
bind = "100.101.102.103:7878"            # the address from `tailscale ip -4`
public-url = "https://laptop.tail1234.ts.net:7878"  # the certificate's name, and the port
tls-cert = "~/.tls/laptop.tail1234.ts.net.crt"
tls-key = "~/.tls/laptop.tail1234.ts.net.key"
```

4. Start the host with `efference host`. It prints an `https://…ts.net:7878` link; opening
   it on the other device signs that browser in.

The host listens only on the Tailscale address, so only the operator's own devices can
reach it. Tailscale's certificates expire: rerunning `tailscale cert` and restarting the host
renews one.

**Behind `tailscale serve`.** The host stays on loopback, and Tailscale serves it to the
tailnet over HTTPS, managing the certificate itself — nothing to renew by hand.

```toml ~/.efference/config.toml
[host]
bind = "127.0.0.1:7878"
public-url = "https://laptop.tail1234.ts.net"   # where Tailscale serves it: port 443
```

Then `tailscale serve --bg http://127.0.0.1:7878` once; the setting persists in Tailscale
until `tailscale serve --https=443 off`. Start the host with `efference host`, and open the
`https://…ts.net` link it prints on the other device. `public-url` is what makes this work:
the host prints and accepts the address browsers use, which here is not the one it listens
on. It gives up HTTP/3, which needs the host to terminate TLS itself; it shares port 443
with anything else the machine serves through Tailscale.

A plain-HTTP bind on anything but loopback is refused, in either arrangement.

## Keeping the host running

A host started in a terminal ends with it. To have it start at login and restart if it
exits, run it under the system's service manager. Either way the service does not see the
operator's shell environment, so model credentials in environment variables will not be
there: use keychain credentials (`efference setup` can move a connection to the keychain)
before making the host a service.

**macOS (launchd)** — `~/Library/LaunchAgents/dev.efference.host.plist`, with the full
path to the `efference` binary (`command -v efference`):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.efference.host</string>
  <key>ProgramArguments</key>
  <array><string>/Users/you/.cargo/bin/efference</string><string>host</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>/Users/you/.efference/host.log</string>
</dict>
</plist>
```

Load it with `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.efference.host.plist`;
`launchctl bootout` with the same arguments stops it.

**Linux (systemd, user unit)** — `~/.config/systemd/user/efference-host.service`:

```ini
[Unit]
Description=efference host

[Service]
ExecStart=%h/.cargo/bin/efference host
Restart=on-failure

[Install]
WantedBy=default.target
```

Start it with `systemctl --user enable --now efference-host`. It runs while the operator is
logged in; `loginctl enable-linger` keeps it running without a login.

A host under a service manager is still the host `efference` finds: projects create
sessions on it rather than starting islands.

## Signing in to the web interface

Being reworked. Today there is one authenticator secret per machine: the link the host
prints signs a browser in, and a six-digit code from an authenticator app does the same
when no link is at hand. While the host has a web bind,
`curl --unix-socket ~/.efference/host.sock http://localhost/auth/otpauth` prints the
enrollment URI to add to an authenticator app.
Revoking access means `efference auth rotate`, which signs out every browser. A way to
sign in a second device by scanning a code, and to revoke one device, is coming; until
then, describe only what is here.
