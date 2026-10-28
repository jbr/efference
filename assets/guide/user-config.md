# The user-wide config: `~/.efference/config.toml`

Every key in the operator's own configuration, and how to use it. One file serves every
project on the machine. `efference://guide/config-format` is the per-project
`efference.toml`; `efference://guide/host` is the host and the web interface in depth.

## Working on this file

The file is in the operator's home directory, outside every session's grants, so reading
or writing it goes to the spotter. Explain what you would change and let the operator agree
before you write it: the spotter clearing a write is not the operator's agreement.

It never holds a credential, and has no key that could: each connection names *where* its
key comes from. Never write a key, token, or password into it, and do not ask the operator
to paste one into the conversation. A key reaches efference through `efference setup`,
which the operator runs in their own terminal, or through an environment variable or
secret store the operator fills.

**`efference setup` owns connections, the default spotter, and the update check.** It
adds, edits, renames, disables, and deletes connections, checks each key with its provider,
and keeps a pasted key in the OS keychain. For any of that, tell the operator to run
`efference setup` rather than editing the connection by hand. It leaves everything else in
the file as written, comments included. Every other key here is edited by hand.

**Edits take effect when a host next starts.** A host reads the file once, at start: in
standalone mode that is the next `efference`; with `efference --host`, the operator stops
the host and starts it again. A restarted host reopens the sessions that were open, idle,
and a standalone host's sessions end with it, so the restart is the operator's to do; do
not stop the host your own session runs in. The terminal UI reads `[tui]` each time it
opens.

**The file is strict.** An unknown key or a value efference cannot read stops the host
from starting, with an error naming the file. A first line of
`#:schema https://efference.net/schemas/user.v1.json`, which `efference setup` writes on a
new file, gives an editor with a TOML language server completion and hover text for every
key. The editor fetches that schema from efference.net; efference itself never reads the
line.

## Connections

A connection is one way to reach models: a provider and where its credential comes from.
A session names its model as `<connection>/<model>`, as in `anthropic/claude-sonnet-4-5`,
so the table's name is part of every model name the operator types.

```toml ~/.efference/config.toml
[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }   # a variable in the host's environment

[connections.work]
provider = "openrouter"
auth = { kind = "keychain" }        # the OS keychain's inference:work item, filled by setup

[connections.chatgpt]
provider = "openai-codex"
auth = { kind = "oauth" }           # a ChatGPT sign-in made in setup

[connections.codex]
provider = "openai-codex"
auth = { kind = "codex-cache" }     # the sign-in the Codex CLI keeps
```

- **`provider`** — one efference knows by name: `anthropic`, `openrouter`, `openai` (the
  platform API), `openai-codex` (the Codex backend, billed to a ChatGPT subscription;
  `codex` is another name for it), or `typesafe` (decision models that can serve as the
  spotter only, never the main model).
- **`auth`** — where the credential comes from, by `kind`:
  - `env` with `key`: an environment variable, read from the host's environment once, when
    it starts. A host run as a service does not have the operator's shell variables.
  - `keychain`: the OS keychain (or the `[secrets]` commands, below). `efference setup`
    stores the key when the operator pastes it. A key replaced there reaches a running
    host the next time the provider refuses the old one.
  - `oauth`: a ChatGPT sign-in made in setup, kept in the keychain and refreshed by the
    host.
  - `codex-cache`: the Codex CLI's own sign-in, in `~/.codex/auth.json`, which the Codex
    CLI must be used often enough to keep fresh.
  - `keychain` and `oauth` take an optional `name` for the stored item; it defaults to the
    connection's name.
- **`disabled = true`** — keep the connection written down without offering it to any
  session. A host refuses to start while any connection that is not disabled cannot load
  its credential, and names each one, so this is how a stale connection stops blocking a
  launch.
- **`spotter-model`** — the spotter model for sessions whose main model is on this
  connection, when neither the session nor `[defaults]` names one (below).

### An endpoint efference has no name for

A server that speaks one of the same APIs is described by its API and address instead of a
`provider`:

```toml ~/.efference/config.toml
[connections.mistral]
dialect = "completions"
base-url = "https://api.mistral.ai/v1"
auth = { kind = "env", key = "MISTRAL_API_KEY" }

[connections.ollama]
dialect = "completions"
base-url = "http://localhost:11434/v1"   # a local server: no auth, no credential sent
```

- **`dialect`** — `messages` (Anthropic Messages), `responses` (OpenAI Responses),
  `completions` (OpenAI Chat Completions), or `systemone` (TypeSafe's decision API, spotter
  only). Takes no `provider`.
- **`base-url`** — required with `dialect`. An `http://` address is reached in clear text,
  credential included, so keep that to local servers.
- **`auth`** may be left out on a described endpoint; then no credential is sent.

A `completions` endpoint takes two more keys, for fields vendors spell differently. Either
one on another dialect stops the host from starting.

```toml ~/.efference/config.toml
[connections.openai-chat]
dialect = "completions"
base-url = "https://api.openai.com/v1"
auth = { kind = "env", key = "OPENAI_API_KEY" }
output-cap-field = "max_completion_tokens"    # the default is "max_tokens"
body = { parallel_tool_calls = false }        # merged into every request
```

- **`output-cap-field`** — the request field that carries the output limit:
  `max_tokens` (the default, which most servers read) or `max_completion_tokens` (OpenAI's
  own Chat Completions).
- **`body`** — fields merged into every request, for a vendor parameter with a fixed value.
  A field efference writes itself is refused, effort included: effort is set per session.

## The three roles and `[defaults]`

Three roles run models in a session. The **main model** is the agent itself. The
**spotter** reviews tool calls the project's grants do not clear. **Triage** reads each
tool result and keeps the lines that matter.

```toml ~/.efference/config.toml
[defaults]
spotter = "anthropic/claude-haiku-4-5"

[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }
```

- **Main model**: no configured default. An omitted model takes past picks (this project's,
  else the machine's), else creation refuses and the new-session form names one.
- **Spotter**: the first of the model the session names, `[defaults] spotter`, the main
  connection's `spotter-model`, a decision model built for the job when a `typesafe` or
  `openrouter` connection exists, and the main provider's small model. `[defaults] spotter`
  must name a connection that exists and is not disabled, or the host refuses to start;
  `efference setup`'s "Default spotter" entry sets it and follows a renamed connection.
- **Triage**: shares the main model unless the session names another.

## `env_hook` — the environment sessions start with

A session created from a terminal (the TUI's form, `efference control create`) starts with
that terminal's environment, and one created by another session carries its creator's.
A session that starts with no client environment takes the host's instead: one created in
the web interface, one resumed (from `efference`'s menu or the web interface), and one a
host reopens when it starts. With no hook, that is the environment the host process started
with — a terminal's, set up for whatever directory it was in, or a service manager's, often
with a `PATH` that lacks the operator's tools.

The symptom to recognize: commands that worked in a session created from a terminal fail
with "command not found", or a project variable is missing, after the session is resumed or
when it was created in the browser. `env_hook` is the fix.

`env_hook` is an argv — a program and its arguments — that prints the environment a session
should start with. It runs in the session's project directory (a session in a worktree
uses the project's), each time one of those sessions starts, with the host's environment.
It is one setting for every project, so it must succeed in a project that has none of its
tool's files.

```toml ~/.efference/config.toml
# direnv: the project's .envrc, or the nearest one above it; unchanged where none applies.
# Covers tools that set a project up through .envrc: nix-direnv's `use flake`,
# devenv's `use devenv`, `devbox generate direnv`, Flox's `use flox`.
env_hook = ["direnv", "exec", ".", "env", "-0"]
```

```toml ~/.efference/config.toml
# mise: the tools and [env] from the project's mise config; unchanged where there is none.
env_hook = ["mise", "exec", "--", "env", "-0"]
```

```toml ~/.efference/config.toml
# The operator's login shell: whatever their startup files set (PATH lines, nvm, conda).
# zsh reads .zshrc only with -i; bash -l reads .bash_profile or .profile.
env_hook = ["/bin/zsh", "-l", "-c", "env -0"]
```

```toml ~/.efference/config.toml
# A login shell that then runs direnv, for a host whose PATH lacks direnv.
env_hook = ["/bin/zsh", "-l", "-c", "direnv exec . env -0"]
```

```toml ~/.efference/config.toml
# zsh with .zshrc, when a startup file prints something: the shell's own output goes to
# /dev/null and only the environment comes back, on file descriptor 3.
env_hook = ["/bin/sh", "-c", "exec /bin/zsh -l -i -c 'env -0 >&3' 3>&1 >/dev/null"]
```

fish is `["fish", "-l", "-c", "env -0"]`; Nushell is `["nu", "-l", "-c", "^env -0"]`, since
Nushell has no `env` command of its own. Shim-based version managers — asdf, pyenv, rbenv,
nodenv, Volta — need no hook: a shim picks the version from the version file in the
directory a command runs in, so they need only their shims directory on the session's
`PATH`, which a login-shell hook supplies when the operator's profile adds it.

What a hook must do, and what goes wrong:

- **No shell unless one is named.** The argv runs directly: no globbing, no `~`, no `$VAR`,
  no pipes. Shell syntax needs a shell in the array, as above.
- **The program is found on the host's `PATH`.** A service's `PATH` may not have
  `/opt/homebrew/bin` or `~/.local/bin`; then no session starts, with "could not run
  env_hook program". Use the full path, or start it from a login shell.
- **The output is `env -0`'s**: `NAME=value` records, each ended by a NUL byte, and it
  replaces the host's environment rather than adding to it. Plain `env` (newline-ended)
  fails.
- **Anything a startup file prints lands in the output.** Text printed before the dump
  becomes part of the first variable's name, silently; that variable is then missing from
  the session. The `>&3` form above avoids it.
- **A failure stops the session from starting**, with nothing falling back: a nonzero exit,
  a missing program, output that is not an environment, more than 30 seconds, or more than
  1 MiB. The reason names the hook (``env: env_hook `direnv` exited 1``); the hook's error
  output is discarded. The common nonzero exits are a `.envrc` the operator has not allowed
  (`direnv allow` in the project) and an untrusted mise config (`mise trust`). Building a
  Nix environment the first time can pass 30 seconds; nix-direnv caches it, so enter the
  project once in a shell first.
- **It does not change model credentials**, which the host reads from its own environment
  at start. Variables a connection reads its key from are removed from the agent's commands
  whatever the hook prints, and the project's `[env]` policy applies to the result.
- `env_hook = []` is refused; omit the key for no hook.

To check a hook, run it in a project and look at the exit status and the variable *names*,
never the values, which would put every secret in the environment into the transcript:

```sh
cd /path/to/project && direnv exec . env -0 >/dev/null; echo "exit $?"
cd /path/to/project && direnv exec . env -0 | tr '\0' '\n' | cut -d= -f1 | sort
```

## `[secrets]` — where efference keeps what it writes

efference stores a few secrets of its own: keys pasted into `efference setup`, ChatGPT and
MCP server sign-ins, and the web interface's root secret. By default they go in the OS
keychain (the login keychain on macOS; the Secret Service on Linux). A Linux server usually
has none a host can open, so `[secrets]` names commands that keep them instead. With the
table present, the keychain is never opened, and `kind = "keychain"` connections read
through the commands too.

```toml ~/.efference/config.toml
[secrets]
get    = ["pass", "show", "efference/{name}"]
set    = ["pass", "insert", "--multiline", "--force", "efference/{name}"]
delete = ["pass", "rm", "--force", "efference/{name}"]
absent_status = 1
```

```toml ~/.efference/config.toml
[secrets]
get    = ["aws", "ssm", "get-parameter", "--with-decryption", "--query", "Parameter.Value", "--output", "text", "--name", "/efference/{name}"]
set    = ["aws", "ssm", "put-parameter", "--type", "SecureString", "--overwrite", "--name", "/efference/{name}", "--value", "file:///dev/stdin"]
delete = ["aws", "ssm", "delete-parameter", "--name", "/efference/{name}"]
absent_status = 254
```

- **`get`** (required) prints the item's value on stdout; one trailing newline is trimmed.
- **`set`** (required) stores it, replacing any previous value; the value arrives on stdin
  with no newline added.
- **`delete`** (optional) removes it. Without it, the few operations that remove an item
  (an MCP server's `--reauth`, a connection renamed or moved in setup) fail and name the
  item for the operator to remove by hand.
- **`{name}`** in an argument becomes the item's name (`inference:<connection>`,
  `oauth:<connection>`, `mcp:<server>`, `root`). Each of `get`, `set`, and `delete` must
  contain it. The value never appears in an argument.
- **`absent_status`** — the exit status `get` gives for an item that does not exist. Without
  it, a first sign-in has nowhere to go, since reading an item not yet stored is an error.
- The commands run directly, with the host's environment and no time limit; a service's
  shorter `PATH` may need full paths. A host does not create the web interface's root secret
  through them: the operator runs `efference auth rotate` once before the first web bind.
- Add the table before the operator runs `efference setup` on such a machine, or setup has
  nowhere to keep a pasted key. Keys in environment variables need neither.

## `[host]` — the web interface

Where a host started with `efference --host` serves the web interface. Without `bind`, it
serves only its Unix socket. A flag beside `efference --host` overrides each key for one
launch.

```toml ~/.efference/config.toml
[host]
bind = "127.0.0.1:7878"
```

- **`bind`** — `ADDR:PORT`. Plain HTTP is refused on any address but loopback.
- **`public-url`** — the address browsers open, `https://name[:port]`: the link the host
  prints, and the host name requests must carry. Required with TLS; on a loopback bind it defaults to
  `http://localhost:<port>`.
- **`tls-cert`**, **`tls-key`** — PEM files, both or neither; a leading `~/` is the home
  directory.

Reaching it from a phone or another computer, over Tailscale, is in
`efference://guide/host`.

## `[updates]` — checking for a newer efference

```toml ~/.efference/config.toml
[updates]
check = true
```

- **`check`** — `true`: a host asks efference.net when it starts, and about once a day
  after, whether a newer version of efference is out, and the terminal UI and web client
  say so in their status line. The answer is believed only when it is signed with
  efference's release key. Nothing is installed; the install command
  (`curl -fsSL https://efference.net/install.sh | sh`) updates. Absent or `false`, a host
  never checks. `efference setup` asks the operator and writes the answer, and its "Update
  check" entry changes it; the choice is the operator's, so do not set it on their behalf.
  `efference update --check` asks once whenever it is run, whatever this key says.

## `[tui]` — how the terminal UI draws

```toml ~/.efference/config.toml
[tui]
theme = "Nord"
images = "kitty"
```

- **`theme`** — a built-in theme's name (`"Catppuccin Frappe"`, the default; `"Nord"`;
  `"gruvbox-dark"`; others) or a path to a `.tmTheme` file; `~/` is the home directory. An
  unknown name lists the built-in ones when the TUI opens. The TUI's theme picker changes
  the theme only until it quits; this key is what keeps one.
- **`images`** — how images in tool results are drawn: `"auto"` (the default) asks the
  terminal and falls back to half-block characters; `"kitty"` and `"iterm2"` name a
  protocol for a terminal detection missed, as under tmux; `"off"` shows each image as a
  one-line reference.
- **`frame-cost`** — times each paint and prints the distribution on exit. Instrumentation
  for developing efference's renderer.
