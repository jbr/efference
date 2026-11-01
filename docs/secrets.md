# The secret store

`src/platform/secrets.rs`. One primitive — `get(name)`, `set(name, value)`, `delete(name)`
of a string — over the user's OS credential store, every item filed under the
service `efference` with the name as its account, or over commands the user
configuration names (`src/platform/secrets/commands.rs`). It is the machine-written
counterpart to the `[env]` table's human-managed references ([env.md](env.md)):
setup writes an API key here, the MCP authorization flow writes its grants here,
and a host reads both at launch.

## Names

Four populations share the store, three under prefixes, so a listing in the OS's own UI
reads and none can shadow another:

| name | value | written by | read by |
| --- | --- | --- | --- |
| `inference:<connection>` | the provider API key | `efference setup` | `InferenceRegistry` at host launch, for `auth = { kind = "keychain" }` ([transport.md](transport.md)) |
| `oauth:<connection>` | the ChatGPT `Grant` record as JSON | `efference setup`, and each refresh | `InferenceRegistry` at host launch, for `auth = { kind = "oauth" }` ([transport.md](transport.md)) |
| `mcp:<server>` | the `StoredAuth` record as JSON | the authorization flow, and each refresh | session boot, before mounting, and a mounted server's renewal ([mcp-auth.md](mcp-auth.md)) |
| `root` | 32 random bytes, base64url | the first `--host --bind` on the machine, except through `[secrets]` commands; `efference auth rotate`, which writes a fresh one | every bind, to derive the web credentials ([web-bind.md](web-bind.md)) |

`inference:` and `oauth:` are deliberately separate prefixes for the same
connection: the first is a bare secret the operator placed, the second a record
efference obtained and rewrites. A person reading the store should not have to
guess which a given item is.

Items are per user, not per project. A grant belongs to the operator and the
server; a second checkout of the same project reuses it rather than registering
another client.

## Backends

`Secrets::configured` picks one per process from the user configuration's `[secrets]`
table (`~/.efference/config.toml`), which its caller reads and hands it
(`InferenceConfig::secret_commands`), and contacts none of them:

- **`[secrets]` commands** — when the configuration has the table, and then the
  platform store is never opened. Described below.
- **macOS, identity-signed binary** — the login keychain in-process, through
  `keyring-core` and the Apple keychain store. A keychain item's access list
  names the code that stored it by its designated requirement. For a binary
  signed with a certificate (any certificate: a self-signed one from Keychain
  Access serves) that requirement is the identifier plus the certificate, stable
  across rebuilds. "Identity-signed" is `SecCode::for_self` validated against
  `certificate leaf[subject.CN] exists`.
- **macOS, ad-hoc** — what the linker gives every `cargo build`; the designated
  requirement is the code hash, so each build would be a new reader and prompt.
  Instead the binary shells out to `security(1)`, whose own access-list entry is
  what every build shares: an item `security` wrote, `security` reads with no
  prompt. Writes go through `security -i`, which takes its command on stdin, so
  a value never sits on a command line. `find-generic-password` exiting 44 is
  "no such item".
- **Linux** — Secret Service in-process over zbus. Nothing there binds an item
  to the binary, so the choice does not arise.

The two macOS backends do not share access-list entries. A signed binary reading
an item a dev build wrote gets one prompt, and "always allow" then sticks; the
reverse direction prompts through `security`. In practice one set is
development's and the other a release's.

### `[secrets]` commands

For a machine whose operator keeps secrets somewhere efference has no client
for, or where no Secret Service runs (a server): AWS SSM or Secrets Manager,
Vault, `pass`, 1Password's or Bitwarden's CLI, `systemd-creds`. The table names
argv arrays, run directly with no shell:

```toml
[secrets]
get    = ["pass", "show", "efference/{name}"]
set    = ["pass", "insert", "--multiline", "--force", "efference/{name}"]
delete = ["pass", "rm", "--force", "efference/{name}"]   # optional
absent_status = 1                                        # optional
```

- **The name is an argument, the value never is.** `{name}` in any argument is
  replaced by the item name. The value goes to `set` on stdin, verbatim, and
  comes back from `get` on stdout with one trailing newline trimmed, since most
  tools end what they print with one. A value on a command line would be visible
  to every process on the machine. The parser refuses a command with no `{name}`,
  which would store every item as one.
- **Absence is declared, never inferred.** `get` exiting 0 is the value.
  `absent_status`, when set, is the exit status the tool gives for "no such
  item"; any other non-zero exit is an error carrying the command's stderr. With
  no `absent_status`, a missing item is an error too. `delete` exiting with it
  succeeds, as deleting an absent item does on every backend. `security`'s exit
  44 is the same idea.
- **`delete` is optional.** Without it a removal fails, naming the item to remove
  by hand: an MCP server's `--reauth`, or a connection renamed or moved out of the
  store in setup. `efference auth rotate` overwrites the root, so it needs none.
- **No root is minted through it.** An absent status a tool shares with other
  failures (`secret-tool` gives 1 for both, `op` 1 for any error, `aws` 254 for
  any error the service returns) could make a transient failure read as
  absence, and a mint over it would sign out every device. So a bind under this
  backend with no `root` fails, naming `efference auth rotate`, which writes one
  ([web-bind.md](web-bind.md)). OAuth and MCP grants keep the absent path of
  every backend, whose worst case is signing in again.

Each operation runs its command once, with stdin closed except for `set`'s
value: a host's stdin may be an island's ownership pipe
([operating.md](operating.md)). The backend caches nothing, so a command runs
whenever a caller reads: for a host, at launch for each connection whose key or
sign-in is stored, at each session boot that mounts an HTTP MCP server, before
each refresh, and when a provider refuses a stored key
([transport.md](transport.md)).

## When the store is opened

Every host start and CLI entry calls `Secrets::configured`, whether or not
anything there reads an item, and a session uses its host's store
(`SessionInference` carries the one its credentials came from), so choosing
only reads the configuration. Under `[secrets]` there is nothing to open: each
operation runs its command. The in-process store (the Secret Service, or the
keychain for a signed binary) is opened by the first `get`, `set` or `delete`
in the process; on Linux that is the D-Bus dial. A host whose connections all
read their keys from the environment, and
that has no `--bind`, starts without opening it, so it starts on a machine with
no Secret Service. A session opens it when it mounts an HTTP MCP server, because
the mount reads the server's grant first ([mcp-auth.md](mcp-auth.md)); a stdio
server is sent no grant, so its mount reads nothing.

A store that cannot be opened fails the operation that needed it, and the
error names that operation's item — ``could not read secret
`inference:anthropic`: the Secret Service (login collection) could not be
opened: …`` — which the host's launch check prefixes with the connection. An
open that fails is not remembered: the next operation tries again, so a store
that comes up later serves a host that is already running. One that succeeds
stays open for the life of the process; concurrent first operations wait on
one open rather than each dialing.

Setup is the exception: it opens the store when it starts, through
`Secrets::open`, so the choice to keep a pasted key there can be listed as
unavailable, with the store's reason, before anyone pastes one. The store is
the one the file setup edits names.

The Secret Service store works when three things hold: a D-Bus session bus, a
daemon serving the Secret Service API on it (GNOME's keyring, KDE's wallet,
KeePassXC), and the collection unlocked. A desktop login has all three. A
systemd user service usually has the bus (the user's systemd instance provides
it at `$XDG_RUNTIME_DIR/bus`) and lacks the other two on a server: no keyring
daemon is installed, or its login collection is encrypted with the login
password and unlocked by PAM at a desktop login, which a lingering service
never has.

Native calls, and the open, run on a fresh thread: the Secret Service store
drives its own async runtime, which cannot be entered from a tokio worker.

## What the store does not protect against

Any process running as the user can read an item through `security` or the
Secret Service, and a signed binary's items are readable by that binary
wherever it is invoked from. Under `[secrets]`, any such process can run the
same `get`, past whatever unlock the command's own tool asks for. So the
agent's own bash tool can read what the harness can: the guard is the
permission gate on those invocations and the masker's needle set, not the
store. The needles are the credentials the session's model roles use, not every
item: MCP grants and the web-bind root are not masked ([env.md](env.md)). That
is the same floor `op read` sits on, and it is above a file: the value is
encrypted at rest, unlocked with the session, and never in a diff. Under
`[secrets]`, how the value is kept at rest is the tool's.

## Failure

An unreadable or missing item is loud and names the item, never the value: a
connection whose `inference:<name>` is absent carries that reason and fails the
sessions that select it, and a store that cannot be opened fails each
operation that needed it, in the same words with the store's reason. A failed
command reads ``could not read secret `oauth:codex`: `aws` exited with status
255: <its stderr>``. Setup lists the "paste it now" choice as unavailable, with
the store's own reason, when there is none to write. A locked keychain prompts
through the OS as any keychain read does; a host with no way to prompt (a
service with no session) fails the read instead of hanging. A command that
waits, such as one that prompts, holds its operation until it exits: no
timeout is applied.
