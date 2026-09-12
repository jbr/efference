# The secret store

`src/secrets.rs`. One primitive — `get(name)`, `set(name, value)`, `delete(name)`
of a string — over the user's OS credential store, every item filed under the
service `efference` with the name as its account. It is the machine-written
counterpart to the `[env]` table's human-managed references ([env.md](env.md)):
setup writes an API key here, the MCP authorization flow writes its grants here,
and a host reads both at launch.

## Names

Three populations share the store, two under prefixes, so a listing in the OS's own UI
reads and none can shadow another:

| name | value | written by | read by |
| --- | --- | --- | --- |
| `inference:<connection>` | the provider API key | `efference setup` | `InferenceRegistry` at host launch, for `auth = { kind = "keychain" }` ([transport.md](transport.md)) |
| `mcp:<server>` | the `StoredAuth` record as JSON | the authorization flow, and each refresh | session boot, before mounting ([mcp-auth.md](mcp-auth.md)) |
| `root` | 32 random bytes, base64url | the first `host --bind` on the machine; `efference auth rotate` deletes it | every bind, to derive the web credentials ([web-bind.md](web-bind.md)) |

Items are per user, not per project. A grant belongs to the operator and the
server; a second checkout of the same project reuses it rather than registering
another client.

## Backends

`Secrets::detect` picks one per process:

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

Native calls run on a fresh thread: the Secret Service store drives its own
async runtime, which cannot be entered from a tokio worker.

## What the store does not protect against

Any process running as the user can read an item through `security` or the
Secret Service, and a signed binary's items are readable by that binary
wherever it is invoked from. So the agent's own bash tool can read what the
harness can: the guard is the permission gate on those invocations and the
masker's needle set, which holds every loaded value, not the store. That is the
same floor `op read` sits on, and it is above a file: the value is encrypted at
rest, unlocked with the session, and never in a diff.

## Failure

An unreadable or missing item is loud and names the item, never the value: a
connection whose `inference:<name>` is absent carries that reason and fails the
sessions that select it, and a store that cannot be opened at all fails the
host launch. Setup lists the "paste it now" choice as unavailable, with the
store's own reason, when there is none to write. A locked keychain prompts
through the OS as any keychain read does; a host with no way to prompt
(a service with no session) fails the read instead of hanging.
