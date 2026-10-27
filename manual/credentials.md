# Credentials

## What you need to know

efference keeps your model keys out of its own files. `~/.efference/config.toml` records
only *where* each key comes from; it has no setting meant to hold a key. A key comes from
one of two places:
<!-- src: src/inference.rs::ConnectionConfig, AuthConfig (deny_unknown_fields; no literal-key field); src/setup/render.rs::HEADER -->

- **Your OS keychain** — the login keychain on macOS, the Secret Service (the desktop's
  password store) on Linux. `efference setup` stores a key there when you paste it in.
  This is the simplest choice, and the one to use if the host runs as a login service.
- **An environment variable**, read once, when the host starts. This suits a secrets
  manager that fills in variables for one command, such as 1Password's `op run` — for
  example, if you want to approve each launch that can spend money on inference.
<!-- src: src/inference/credential.rs::load_credential; src/secrets.rs::Secrets::detect, native::DESCRIPTION; src/setup/flow.rs::api_key_source, keychain_source -->

A ChatGPT subscription has no key: setup either signs you in through the browser and keeps
that sign-in in the keychain, or reuses the sign-in the Codex CLI already has.
<!-- src: src/setup/flow/chatgpt.rs::chatgpt_source, oauth_source, codex_cache_source -->

Run `efference setup` again at any time to change a connection: replace its key, move an
environment-variable key into the keychain, or take a key out of the keychain.
<!-- src: src/setup/manage.rs::run; src/setup/manage/credential.rs::edit_connection -->

### Keys in the keychain

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
<!-- TODO: blocked on backlog/linux-without-secret-service.md — today a Linux host with no Secret Service fails to start even with environment-variable keys. -->

### Keys in environment variables, with `op run`

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

## How this works, precisely

### What the agent can see

The agent's commands do not inherit the host's model keys: every variable a connection
reads its key from is removed from the environment of the commands a session runs. A
project can opt one back in, for a project that calls the provider itself.
<!-- src: src/boot/env.rs::build_env_policy; src/env_table.rs::Table::merge_with_host; src/env_table/child.rs::ChildEnv::apply -->

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

The agent's commands run as you. Programs running as you can read the host's environment
through the operating system, and on Linux can read the Secret Service once it is
unlocked; a command that reads a keychain item directly passes through the same permission
checks as any other ([Permissions](permissions.md)).
<!-- src: src/secrets.rs (module doc); src/policy.rs (no keychain special case) -->
<!-- TODO: macOS item access depends on release signing (src/secrets.rs::identity_signed). -->

### What is stored where

| Where | What | Secret? |
| --- | --- | --- |
| OS keychain, service `efference` | `inference:<connection>` (a pasted key), `oauth:<connection>` (a ChatGPT sign-in), `mcp:<server>` (an MCP server sign-in), `root` (the secret every web sign-in and the notification signing key derive from) | Yes |
| `~/.efference/config.toml` | Connections, where each key comes from, the `[host]` table. Setup creates it readable only by you. | No |
| `~/.efference/state.db` | Model choices per directory, model lists, the projects and sessions efference has run, each notifying browser's push subscription, the devices signed in to the web address, the browsers waiting to sign in | No model keys; the push subscriptions are per-browser secrets |
| `<project>/.efference/<session>/` | The session's log, its configuration, its usage record, and attached files | Your project's content and the agent's work |
| `<project>/efference.toml` | The project's settings. A value written here literally is stored in plain text, and copied into each session's directory. | Only if you put one there; use an `op://` reference instead |
<!-- src: src/secrets.rs::inference_name, oauth_name, mcp_name; src/auth.rs::ROOT_ITEM (module doc); src/setup/render.rs::write (0600 on create); src/state.rs; src/history.rs; src/catalog/store.rs; src/remembered.rs; src/session_ids.rs; src/push.rs::SCHEMA; src/devices.rs::SCHEMA; src/sign_ins.rs::SCHEMA; src/boot.rs::LOG_NAME, CONFIG_NAME; src/daemon/metrics/usage_line.rs; src/attachments.rs; src/boot/scaffold.rs -->
<!-- TODO: blocked on backlog/local-file-modes.md — today session directories and logs are created with default permissions (readable by other local accounts under a 022 umask). State the modes once that ships. -->

A ChatGPT connection that reuses Codex's sign-in reads Codex's own file,
`~/.codex/auth.json`; efference never writes it.
<!-- src: src/api/endpoint.rs::codex_cached_credentials, codex_auth_path -->

Two places a key can still reach a file by accident: setup's "Variable name" prompt
accepts any text, so a key pasted there is written to `config.toml` as a variable name;
and a provider's error message, printed by the host, lands in a standalone host's log file
at `~/.efference/islands/<pid>/host.log` if the provider quotes the key back.
<!-- src: src/setup/flow.rs::api_key_source (only empty or '=' rejected); src/api/client/send.rs::open_error; src/bin/efference/island.rs::Island::launch (stderr to host.log) -->
<!-- Not planned (backlog/parked/config-grant-written-before-masking.md): an approved grant is written to the session config before masking; a secret in a command has already reached the provider. -->
