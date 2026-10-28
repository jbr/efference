# Credentials

You can connect a model without putting its API key in an efference configuration file.
Setup records *where* to find the key, not the key itself. Choose the storage that fits
how you run efference: a desktop keychain, environment variables supplied by a secrets
manager, or secret commands for a server without a keychain.
<!-- src: src/inference.rs::ConnectionConfig, AuthConfig; src/secrets.rs::Secrets::configured; src/secrets/commands.rs::SecretCommands; src/setup/render.rs::HEADER -->

## Choose how to sign in

For an API key, start with one of these two options:
<!-- src: src/inference.rs::ConnectionConfig, AuthConfig (deny_unknown_fields; no literal-key field); src/setup/render.rs::HEADER -->

- **Your OS keychain** — the login keychain on macOS. On desktop Linux it is the Secret
  Service your desktop already runs: GNOME's keyring, KDE's wallet, or KeePassXC.
  `efference setup` stores a key there when you paste it in. This is the simplest choice,
  and the one to use if the host runs as a login service.
- **An environment variable**, read once, when the host starts. This suits a secrets
  manager that fills in variables for one command, such as 1Password's `op run` — for
  example, if you want to approve each launch that can spend money on inference.
<!-- src: src/inference/credential.rs::load_credential; src/secrets.rs::Secrets::configured, detect, native::DESCRIPTION; src/setup/flow.rs::api_key_source, keychain_source -->

Using a ChatGPT subscription instead? You don’t need an API key. Setup can sign you in
through the browser and keep the sign-in in the keychain, or reuse the Codex CLI’s existing
sign-in.
<!-- src: src/setup/flow/chatgpt.rs::chatgpt_source, oauth_source, codex_cache_source -->

Run `efference setup` again at any time to change a connection: replace its key, move an
environment-variable key into the keychain, or take a key out of the keychain.
<!-- src: src/setup/manage.rs::run; src/setup/manage/credential.rs::edit_connection -->

## Use your OS keychain

Choose "Paste it now, kept in the OS keychain" in `efference setup`. Setup checks the key
with the provider where it can, stores it, and records `auth = { kind = "keychain" }` in the
config. A host running as a login service does not see your shell's environment, so a
keychain key is the one that works there.
<!-- src: src/setup/flow.rs::keychain_source, check_key; src/setup/render.rs::auth_inline_table -->

A key replaced in `efference setup` reaches a host that is already running: the next time
the provider refuses the old key, the host reads the keychain again and retries with what
it finds. A refusal for another reason, such as a spending limit, reads nothing.
<!-- src: src/api/failure.rs::rejects_credential; src/api/auth_value.rs; src/inference/credential.rs::load_credential (keychain arm re-reads; env arm has no reloader) -->
<!-- TODO: macOS keychain access prompts depend on how release binaries are signed; describe once dist signing is settled. -->

On a Linux server, there may be no Secret Service, or the keyring may stay locked without
a desktop login. Use [environment variables](#keys-in-environment-variables-with-op-run)
or [secret commands](#secret-commands) there instead. A host with only environment-variable
keys can start without a keychain: efference opens it only when needed. If a connection
needs a keychain item that cannot be read, the host won’t start, and the error names both
the connection and the item.
<!-- src: src/secrets.rs::Secrets::configured, detect, native::open; src/bin/efference/serve.rs::refuse_broken_connections -->

## Keys in environment variables, with `op run`

1Password's command-line tool, `op`, can start a program with secrets filled into its
environment. Keep a *reference* to the key, not the key, in a file:

```sh
# ~/.efference/keys.env — references, not secrets
ANTHROPIC_API_KEY=op://Private/Anthropic/credential
```

The reference names a vault, an item, and a field in 1Password. Start the host under
`op run`, which replaces each reference with the secret for that one process:

```sh
op run --env-file ~/.efference/keys.env -- efference --host
```

The connection in `~/.efference/config.toml` names the variable:

```toml
[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }
```

The host reads the key once, when it starts, and every session it runs uses it. A host keeps
the environment it started with: after changing a key, restart the host.
<!-- src: src/bin/efference/serve.rs::host (EnvSnapshot::capture at start); src/inference/credential.rs (env arm, no reload); src/api/endpoint.rs (op run substitutes a whole-value reference); op run --help (2.39.0) -->

In standalone mode, wrap `efference` itself, with `--no-masking`, because the terminal
interface needs `op run` to leave its output alone:

```sh
op run --no-masking --env-file ~/.efference/keys.env -- efference
```
<!-- src: src/tui/ui_loop.rs (draws to stdout); src/bin/efference/island.rs::Island::launch (the standalone host inherits the launcher's environment); verified by jbr 2026-10-19 -->

efference does not resolve `op://` references in connection settings itself; `op run` (or
something like it) must have done that before the host starts. A project's
`efference.toml` can also give the agent's commands secrets as `op://` references, and
efference resolves those with `op read` when a session starts.
<!-- src: src/inference/credential.rs::load_credential; src/env_table/resolve.rs::SCHEMES, run_command; src/boot/env.rs::resolve_env_table -->

## Secret commands

If you’re setting up a server without a keychain, you can use a password manager’s CLI,
a cloud secrets service, or `systemd-creds` as efference’s secret store. Unlike supplying
only model keys through the environment, this also gives efference somewhere to keep
ChatGPT and MCP server sign-ins and the secret used for browser access.
<!-- src: src/secrets/commands.rs::SecretCommands; src/secrets.rs::Secrets::configured, from_table -->

Add a `[secrets]` table to `~/.efference/config.toml` before setup. It names commands to
read (`get`), store (`set`), and optionally remove (`delete`) a secret. These commands
replace the keychain for all of efference’s stored secrets. Even a connection configured
with `auth = { kind = "keychain" }` uses them when the table is present.
<!-- src: src/secrets/commands.rs::SecretCommands; src/secrets.rs::Secrets::configured, from_table; src/inference.rs::InferenceConfig (secrets); src/setup/probe.rs::LiveProbe::capture -->

Each command is a program and its arguments, run directly rather than by a shell. `{name}`
in any argument becomes the name of the secret, such as `oauth:codex` or `root`. The
secret itself never appears in an argument: `set` receives it on standard input, and `get`
prints it. `absent_status` is the exit status your tool gives when a secret does not
exist; without it, a secret efference has not stored yet reads as a failure, so a first
sign-in has nowhere to go.
<!-- src: src/secrets/commands.rs::SecretCommands::get, set, delete, run, validate -->

### With AWS Systems Manager Parameter Store

Use the AWS CLI to store encrypted parameters:

```toml
[secrets]
get    = ["aws", "ssm", "get-parameter", "--with-decryption", "--query", "Parameter.Value", "--output", "text", "--name", "/efference/{name}"]
set    = ["aws", "ssm", "put-parameter", "--type", "SecureString", "--overwrite", "--name", "/efference/{name}", "--value", "file:///dev/stdin"]
delete = ["aws", "ssm", "delete-parameter", "--name", "/efference/{name}"]
absent_status = 254
```

### With pass

`pass` encrypts each secret with your GPG key. A host running as a service has no
terminal to ask for a passphrase, so the key must have none, or `gpg-agent` must already
hold it:

```toml
[secrets]
get    = ["pass", "show", "efference/{name}"]
set    = ["pass", "insert", "--multiline", "--force", "efference/{name}"]
delete = ["pass", "rm", "--force", "efference/{name}"]
absent_status = 1
```

### With 1Password documents

This uses `op` to keep each secret as a document in a vault named `efference`. The `set`
command edits an existing document or creates it if missing:

```toml
[secrets]
get    = ["op", "document", "get", "--vault", "efference", "{name}"]
set    = ["sh", "-c", 'if op document get --vault efference "$1" </dev/null >/dev/null 2>&1; then exec op document edit --vault efference "$1" -; else exec op document create --vault efference --title "$1" -; fi', "sh", "{name}"]
delete = ["op", "document", "delete", "--vault", "efference", "{name}"]
absent_status = 1
```

### With systemd-creds

`systemd-creds` encrypts each secret in a file under `~/.config/efference/credentials`, so
only your user or root can decrypt it. This example needs systemd 256 or later for the
`--user` option:

```toml
[secrets]
get    = ["sh", "-c", 'f="$HOME/.config/efference/credentials/$1"; [ -e "$f" ] || exit 3; exec systemd-creds --user decrypt "$f" -', "sh", "{name}"]
set    = ["sh", "-c", 'd="$HOME/.config/efference/credentials"; mkdir -p "$d" && exec systemd-creds --user encrypt - "$d/$1"', "sh", "{name}"]
delete = ["sh", "-c", 'exec rm -f "$HOME/.config/efference/credentials/$1"', "sh", "{name}"]
absent_status = 3
```

The 1Password and systemd examples run a short shell script. They pass `{name}` as its
own argument (`$1`) rather than inserting it into the script text. All the commands run
with the host’s environment. A service manager may supply a shorter `PATH` than your
shell; if a program isn’t found, use its full path.
<!-- src: src/secrets/commands.rs::run (argv run directly; {name} replaced per argument); AWS CLI return codes (docs.aws.amazon.com/cli/latest/topic/return-codes.html: 254, the service returned an error); pass(1); op document --help (2.39.0); systemd NEWS, CHANGES WITH 256 ("systemd-creds gained new options --user/--uid="); systemd-creds(1) -->

A host does not create the web address's secret through the commands. Before the first
`efference --host --bind`, run `efference auth rotate` once to store one; until then, a
host with a bind refuses to start and says to run it.
<!-- src: src/auth.rs::Root::load (RootError::Absent), Root::rotate; src/bin/efference/main.rs::rotate -->

## What is protected, and what is stored

### Keeping model keys out of commands

The agent's commands do not inherit the host's model keys: every variable a connection
reads its key from is removed from the environment of the commands a session runs. A
project can opt one back in, for a project that calls the provider itself.
<!-- src: src/boot/env.rs::build_env_policy; src/env_table.rs::Table::merge_with_host; src/env_table/child.rs::ChildEnv::apply -->

### Masking secrets in the record

Before anything is written to a session's log, efference replaces the secret values it
knows with a marker, such as `⟦anthropic (ANTHROPIC_API_KEY): elided by harness⟧` for a
connection's key. The values it knows are:
<!-- src: src/persist.rs::Log::append_submission; src/env_table/mask.rs::Masker::mask_payload, marker, secret_marker -->

- the keys the session's model connections use, however they are stored;
- environment variables whose names end in `_KEY`, `_SECRET`, `_TOKEN`, `_PASSWORD`, or
  `_CREDENTIALS`;
- variables a project marks for masking, and every value it gives as an `op://` reference.

The last two apply to values of at least 8 characters.

A key that is replaced while a session runs, such as a refreshed ChatGPT sign-in or a key
you changed in the keychain, is masked from that moment on, the same way as the original.
<!-- src: src/env_table.rs::BUILT_IN, EnvPolicy::build_session; src/env_table/mask.rs::MASKABILITY_FLOOR, Masker::absorb_secret; src/inference.rs::SessionInference::absorb_secrets -->

A value is also caught in its common encodings: URL-encoded, JSON-escaped, base64. What the
model is sent and what triage reads come from the log, so they carry the masked text too.
While a turn runs, the clients you have open show its output live, before masking; the
record they reload is masked.
<!-- src: src/env_table/mask.rs::needles_for; src/agent/batch.rs (triage reads the entry back from the log); src/daemon/frames.rs (Class::Output stream deltas, pre-append) -->

Masking is a guard against accidents, not against an agent trying to get a secret out. A
secret efference does not know about reaches the log as the agent saw it: a key in a file
the agent reads, a variable named outside the patterns above (`DATABASE_URL`), or a secret
transformed in a way the encodings above do not cover. Signed reasoning blocks from the
model and image data are stored as received. Files you attach to a message are stored in
the session's directory as you sent them.
<!-- src: src/env_table/mask.rs::mask_blocks, mask_payload; src/attachments.rs; src/env_table.rs::EnvPolicy::build_session -->

### Where those protections stop

The agent's commands run as you. Programs running as you can read the host's environment
through the operating system, and on Linux can read the Secret Service once it is
unlocked; a command that reads a keychain item directly passes through the same permission
checks as any other ([Permissions](permissions.md)).
<!-- src: src/secrets.rs (module doc); src/policy.rs (no keychain special case) -->

With secret commands, any program running as you can run the same `get` command,
including a command the agent runs, which passes through the same permission checks. What
stands in its way is whatever your tool asks for before it answers.
<!-- src: src/secrets/commands.rs::run (inherits the host's environment and user); src/policy.rs (no special case) -->
<!-- TODO: macOS item access depends on release signing (src/secrets.rs::identity_signed). -->

### Secret commands, precisely

`set` receives the secret exactly, with no newline added. Of what `get` prints, one
trailing newline is removed, since most tools end their output with one; a secret is
otherwise returned as printed, spaces included.
<!-- src: src/secrets/commands.rs::SecretCommands::get, run -->

`get` exiting 0 means the secret is what it printed, and exiting with `absent_status`
means there is no such secret. Any other exit is an error, and the error efference reports
names the secret, the program, its exit status, and its error output. `delete` exiting
with `absent_status` counts as success: the secret is already gone.
<!-- src: src/secrets/commands.rs::SecretCommands::is_absent, delete, succeeded -->

Many tools give the same exit status for "not found" as for other failures: `op` exits 1
for any error, `pass` exits 1 for several of its own errors as well, and `aws` exits 254
for any error the service returns, such as a refused permission. For a sign-in, that
costs at most a sign-in: a failed read taken for a missing one asks you to sign in again.
For the web address's secret, creating a new one over a read that failed would sign out
every browser, so a host never does; only `efference auth rotate` writes it, and it
writes rather than deletes, so it needs no `delete` command.
<!-- src: src/auth.rs::Root::load, Root::rotate, RootError; src/mcp/auth/credentials.rs::stored_credentials; src/inference/credential.rs::load_credential; password-store.sh (die exits 1; cmd_show); 1password.community "Exit code documentation" (op uses 0 and 1); AWS CLI return codes -->

Without a `delete` command, the few operations that remove a secret fail, naming the
secret for you to remove by hand: `--reauth` for an MCP server, and a connection renamed
in `efference setup` or moved out of the store.
<!-- src: src/secrets/commands.rs::SecretCommands::delete; src/boot/mcp_mount.rs::mount_all (reauth clear); src/setup/manage/credential.rs -->

efference runs `get` each time it reads a secret: when the host starts, for each connection
whose key or sign-in is stored; when a session starts with an MCP server that is reached
over HTTP; before it refreshes a sign-in; and when a provider refuses a stored key. A tool
that asks you to approve each read asks at each of these. efference sets no time limit on
a command.
<!-- src: src/inference/credential.rs::load_credential (keychain reload, oauth reload re-reads); src/boot/mcp_mount.rs::mount_one; src/mcp/auth/credentials.rs::stored_credentials; src/secrets/commands.rs (no cache, no timeout) -->

### What is stored where

| Where | What | Secret? |
| --- | --- | --- |
| OS keychain, service `efference`, or wherever your secret commands keep them | `inference:<connection>` (a pasted key), `oauth:<connection>` (a ChatGPT sign-in), `mcp:<server>` (an MCP server sign-in), `root` (the secret every web sign-in and the notification signing key derive from) | Yes |
| `~/.efference/config.toml` | Connections, where each key comes from, the `[host]` table, the `[secrets]` commands, the environment hook, and the terminal interface's settings ([Configuration](configuration.md)). Setup creates it readable only by you. | No |
| `~/.efference/state.db` | Model choices per directory, model lists, the projects and sessions efference has run, each notifying browser's push subscription, the devices signed in to the web address, the browsers waiting to sign in | No model keys; the push subscriptions are per-browser secrets |
| `<project>/.efference/<session>/` | The session's log, its configuration, its usage record, and attached files | Your project's content and the agent's work |
| `<project>/efference.toml` | The project's settings. A value written here literally is stored in plain text, and copied into each session's directory. | Only if you put one there; use an `op://` reference instead |
<!-- src: src/secrets.rs::inference_name, oauth_name, mcp_name; src/auth.rs::ROOT_ITEM (module doc); src/setup/render.rs::write (0600 on create); src/state.rs; src/history.rs; src/catalog/store.rs; src/remembered.rs; src/session_ids.rs; src/push.rs::SCHEMA; src/devices.rs::SCHEMA; src/sign_ins.rs::SCHEMA; src/boot.rs::LOG_NAME, CONFIG_NAME; src/daemon/metrics/usage_line.rs; src/attachments.rs; src/boot/scaffold.rs -->

A project's `.efference/` directory is readable only by you (mode `0700`), and the files
efference writes in a session's directory — the log, the configuration copy, the usage
record, attached files — are `0600`. The directory is what keeps other accounts out of
everything below it, including session worktrees, whose checked-out files keep their
ordinary modes. An existing `.efference/` is tightened the next time a session starts
or resumes in it, and a stricter mode is left alone.
<!-- src: src/daemon/permissions.rs::ensure_project_base, tighten_project_base; src/boot/mint.rs::mint_session_dir; src/persist.rs::Log::create -->

A ChatGPT connection that reuses Codex's sign-in reads Codex's own file,
`~/.codex/auth.json`; efference never writes it.
<!-- src: src/api/endpoint.rs::codex_cached_credentials, codex_auth_path -->

Two edge cases are worth knowing when you’re checking where keys might end up:

- A provider can quote a key back in an error. In standalone mode, the host’s error
  output goes to `~/.efference/standalone/<pid>/host.log`, so that file could contain it.
- Setup’s “Variable name” prompt expects a name, not a key. It accepts letters, digits,
  and `_`, with no leading digit. If you paste something that looks like a key there,
  setup refuses it without repeating it or writing it to `config.toml`.
<!-- src: src/setup/flow/variable.rs::ask_variable_name; src/api/client/send.rs::open_error; src/bin/efference/island.rs::Island::launch (stderr to host.log) -->
<!-- Not planned (backlog/parked/config-grant-written-before-masking.md): an approved grant is written to the session config before masking; a secret in a command has already reached the provider. -->
