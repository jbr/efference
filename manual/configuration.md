# Configuration

Setup gets you connected to a model without having to write a configuration file. When
you want to adjust how efference runs — prepare project environments, use a different
permission spotter, or change the terminal’s appearance — your settings are here.
<!-- src: src/setup/render.rs::render; src/config/user_config.rs::InferenceConfig; src/config/user_config/tui.rs::TuiConfig -->

## Find the right file

There are two places for settings, depending on whether they belong to you or the project:

- **`~/.efference/config.toml`**, yours, for every project on this machine: how to reach
  model providers, and how efference runs here. This page describes it.
- **`efference.toml`**, in a project: what sessions there are for, what they may do without
  asking you, and what they start out knowing. [Project configuration](project-configuration.md)
  describes it.
<!-- src: src/config/user_config.rs::CONFIG_RELATIVE_PATH, InferenceConfig; src/config/raw.rs::RawConfig -->

For most changes to connections, run `efference setup` again. It can add, edit, rename,
disable, or remove a connection, choose a default spotter model, and change whether
new-version checks are enabled. It preserves the rest of your file, including comments.
<!-- src: src/bin/efference/setup.rs::default_verb; src/setup/render.rs::render, apply_change; src/setup/manage.rs::run; src/setup/updates.rs -->

For the other settings, edit `~/.efference/config.toml` directly. You can also ask a running
session to help: it can read efference’s description of every key at
`efference://guide/user-config`. Edits to this file normally go to the permission spotter
because the file sits outside the project; [Permissions](permissions.md) explains how
review works.
<!-- src: src/bin/efference/setup.rs::default_verb; src/setup/render.rs::render, apply_change; src/setup/manage.rs::run; src/setup/updates.rs; src/config/guide.rs::DOCUMENTS (user-config); src/config/policy/paths.rs::context_granted (the guide prefix is read-granted); absence of a ~/.efference grant in src/config/policy/ -->

A host reads the file when it starts. A change takes effect the next time you run
`efference` in standalone mode, or, with `efference --host`, once you stop the host and
start it again. The terminal interface reads `[tui]` each time it opens. A key efference
does not know, or a value it cannot read, stops it from starting, with an error naming the
file: a misspelled key is never ignored.
<!-- src: src/config/user_config.rs::InferenceConfig (deny_unknown_fields), read_user, from_toml; src/bin/efference/serve.rs::host (InferenceRegistry::load); src/bin/efference/island.rs::Island::launch (a standalone host is a new --host process); src/bin/efference/verbs.rs (TuiConfig::load) -->

## Connect the models you want to use

Each `[connections.<name>]` table is one way to reach a model: a provider efference knows,
or another endpoint that speaks one of the same APIs, and where its key comes from. A
session names its model as `<connection>/<model>`, such as `anthropic/claude-sonnet-4-5`.
`efference setup` writes these, and [Credentials](credentials.md) explains where keys can
come from.
<!-- src: src/config/user_config.rs::InferenceConfig (connections), ConnectionConfig; src/setup/render.rs::render -->

```toml
[connections.anthropic]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_API_KEY" }

[connections.local]
dialect = "completions"
base-url = "http://localhost:11434/v1"   # a local server that takes no key: no auth
```
<!-- src: src/config/user_config.rs::ConnectionConfig; src/inference/registry.rs (a described endpoint with no auth sends no credential); src/setup/render.rs::render (dialect and base-url for a described endpoint) -->

For Ollama, configure the server's context window before using it with an agent:
set `OLLAMA_CONTEXT_LENGTH=64000` when starting `ollama serve`, or set
`PARAMETER num_ctx 64000` in a model's Modelfile. `ollama ps` shows the loaded
context window. Ollama [recommends at least 64k for coding agents](https://docs.ollama.com/context-length);
allow room for the system prompt, tool definitions, conversation, and reply.
A window that is too small can silently discard part of the prompt. efference does
not detect this from reported token counts, so configure the window for both main
and spotter models served locally.

A host refuses to start while any connection's key cannot be read, and names each one.
`disabled = true`, which setup writes when you turn a connection off, keeps a connection in
the file without using it.
<!-- src: src/bin/efference/serve.rs::refuse_broken_connections; src/inference/registry.rs (disabled connections load no credential); src/setup/render.rs::apply_change (SetDisabled) -->

## Give sessions your project environment

If your project uses direnv, mise, or shell startup files to find its tools, you’ll want
those tools available when you start a session from the browser or resume it later.

A session created in the terminal with `efference` or `efference control create` already
has that terminal’s environment: its `PATH` and the variables your project tools loaded.
Sessions without a terminal to copy from get their environment from the host instead:
<!-- src: src/clients/tui/host_dispatch.rs::create_session (EnvSnapshot::capture); src/clients/control_tools/create.rs::Create::execute (Executor::Client captures the caller's environment); src/host/create.rs (an omitted env is established); src/host/preparation.rs::establish_environment -->

- one created in the web client;
- one resumed, from `efference`'s menu or from the web client;
- one a host reopens when `efference --host` starts.
<!-- src: src/host/create.rs (supplied_env None); src/host/resume.rs::PatchSession::activate (env: None), prepare_resume; src/host/boot_resume.rs; web/src/api/sessionForm.ts (sends no env) -->

That can explain a session finding `cargo` or `node` at first, but not after a resume.
Without an `env_hook`, the host passes on the environment it started with — perhaps set up
for a different project, or supplied by a service manager with a `PATH` missing your tools.
<!-- src: src/config/env_table/launch.rs::run_hook (no hook: the host environment as given); src/bin/efference/serve.rs::host (EnvSnapshot::capture once at start) -->

Use **`env_hook`** to prepare the right environment for these sessions. It names a command
that runs in the project directory and prints the environment to use. Pick the example
below that matches your tools. The setting applies to every project, so the command must
also work in projects without that tool’s configuration, as these examples do.
<!-- src: src/config/user_config.rs::InferenceConfig (env_hook); src/config/env_table/launch.rs::run_hook (current_dir); src/host/preparation.rs::establish_environment -->

Before choosing a hook, there are four requirements to know:

- **It is a list, not a command line.** The first item is the program; the rest are its
  arguments, passed as written. Nothing runs a shell unless you name one.
- **The program is looked up on the host's `PATH`.** A host run as a service may not have
  the directory a tool lives in, and then no session starts. Name the program by its full
  path, such as `/opt/homebrew/bin/direnv`, or start it from a login shell (below).
- **It must print `env -0`'s format**, one `NAME=value` record per variable, each ended by
  a NUL byte. Plain `env` ends records with a newline, which fails.
- **A failing hook stops the session from starting.** The reason names the hook and how it
  failed, such as ``env: env_hook `direnv` exited 1``; the hook's own error messages are
  discarded. Run the hook yourself in the project to see them.
<!-- src: src/config/env_table/launch.rs::run_hook (Command::new(program).args(args); stderr null; host_env applied, so PATH is the host's), LaunchError (Display: Spawn, Exited, Unterminated); src/host/preparation.rs::establish_environment ("env: …"), preparation_failed -->

### With direnv, Nix, devenv, devbox, or Flox

```toml
env_hook = ["direnv", "exec", ".", "env", "-0"]
```

direnv applies the `.envrc` in the project directory or the nearest directory above it;
where none applies, the environment passes through unchanged. It refuses a `.envrc` you
have not allowed, and the hook then exits 1, so run `direnv allow` in each project first.
<!-- src: direnv 2.37.1, checked 2026-10-27: an allowed .envrc applied; a parent's .envrc applied from a subdirectory; a blocked .envrc prints "is blocked" and exits 1 -->

Several environment tools set a project up by writing its `.envrc`, and this hook covers
them too: Nix flakes through nix-direnv (`use flake`), devenv (`use devenv`), devbox
(`devbox generate direnv`), and Flox (`use flox`). Building a Nix environment the first
time can take longer than the hook's 30 seconds. nix-direnv caches it, so enter the project
in your shell once before a session needs it.
<!-- src: github.com/nix-community/nix-direnv README (use flake; caches the environment after the first run); devenv.sh/integrations/direnv (use devenv); jetify.com/docs/devbox/ide-configuration/direnv (devbox generate direnv); flox.dev/docs/concepts/activation (use flox); src/config/env_table/launch.rs::HOOK_TIMEOUT -->

### With mise

```toml
env_hook = ["mise", "exec", "--", "env", "-0"]
```

mise applies the tools and the `[env]` table from the project's mise configuration; a
project without one gets the environment unchanged. mise refuses a configuration file you
have not trusted, and the hook then exits 1, so run `mise trust` in each project first.
<!-- src: mise 2026.4.15, checked 2026-10-27: [env] applied from a trusted mise.toml; no config passes through; an untrusted mise.toml prints "are not trusted" and exits 1 -->

### With asdf, pyenv, rbenv, nodenv, or Volta

These need no hook. Their commands are *shims*, small programs on the `PATH` that pick a
version each time they run, from the version file (`.tool-versions`, `.python-version`, …)
in the directory they run in. The agent's commands run in the project, so a session gets
the project's versions as long as the shims directory, such as asdf's `~/.asdf/shims`, is
on its `PATH`. A host started from your shell has it already. For a host run as a service,
use a login-shell hook, below, if your shell's startup files add it.
<!-- src: asdf-vm.com/guide/getting-started (PATH line for ${ASDF_DATA_DIR:-$HOME/.asdf}/shims); asdf-vm.com/manage/configuration (.tool-versions applies in its directory and subdirectories); pyenv 2.8.4 checked 2026-10-27: the same shim reported a different version in a directory with a .python-version -->

### With your shell’s startup files

For anything your shell sets up when it starts, such as a `PATH` line, nvm, or
`conda init`, run your shell as a login shell:

```toml
env_hook = ["/bin/zsh", "-l", "-c", "env -0"]    # zsh
env_hook = ["/bin/bash", "-l", "-c", "env -0"]   # bash
env_hook = ["fish", "-l", "-c", "env -0"]        # fish
env_hook = ["nu", "-l", "-c", "^env -0"]         # Nushell
```
<!-- src: checked 2026-10-27 on macOS: each prints NUL-terminated records and exits 0 (zsh 5.9, bash 3.2, fish 4.8.1, nu 0.112.2; Nushell has no env command, so ^env runs the external one) -->

A login zsh reads `.zshenv`, `.zprofile`, and `.zlogin`, but not `.zshrc`, which is read
only by an interactive shell; to include it, add `-i`. Anything a startup file prints is
read as part of the environment and garbles it, which is likelier with `-i`. This form
sends the shell's own output away and only the environment back:

```toml
env_hook = ["/bin/sh", "-c", "exec /bin/zsh -l -i -c 'env -0 >&3' 3>&1 >/dev/null"]
```
<!-- src: zsh(1) STARTUP/SHUTDOWN FILES; checked 2026-10-27: a .zshrc that echoes a line left the first record named "welcome back\nSHELL" with the plain form, and no stray record with the fd 3 form; src/config/env_table/snapshot.rs::EnvSnapshot::new (a name with a newline is accepted) -->

A login shell can also start one of the tools above when the host's `PATH` lacks it:

```toml
env_hook = ["/bin/zsh", "-l", "-c", "direnv exec . env -0"]
```
<!-- src: checked 2026-10-27: exits 0 with NUL-terminated records -->

The hook decides what the agent's commands see, not where model keys come from: the host
reads keys from its own environment, when it starts.
<!-- src: src/inference/credential.rs::load_credential (env arm reads the host's start-time environment); src/bin/efference/serve.rs::host -->

## Store secrets on a server: `[secrets]`

On a machine with no OS keychain, usually a Linux server, `[secrets]` names commands that
keep efference's secrets instead: keys pasted into `efference setup`, ChatGPT and MCP server
sign-ins, and the web address's secret. Add it before running `efference setup` there if you
want setup to keep a key for you; keys in environment variables need neither.
[Credentials](credentials.md) has the details and examples for AWS, `pass`, 1Password, and
`systemd-creds`.
<!-- src: src/platform/secrets/commands.rs::SecretCommands; src/platform/secrets.rs::Secrets::configured; src/setup/flow.rs::api_key_source (the keychain choice is unavailable without a store) -->

## Open sessions in a browser: `[host]`

`[host]` turns on the web client: where a host started with `efference --host` listens for
browsers, and with which certificate. [Remote access](remote-access.md) covers it.
<!-- src: src/config/user_config/host.rs::HostConfig -->

## Choose a default spotter: `[defaults]`

`[defaults] spotter` names the model that reviews the agent's tool calls when a session
doesn't name one, as `<connection>/<model>`. Setup's "Default spotter" entry sets it. A
connection's own `spotter-model` key names one for sessions whose main model is on that
connection. [Permissions](permissions.md) has the full order.
<!-- src: src/config/user_config.rs::Defaults, ConnectionConfig (spotter_model); src/setup/defaults.rs; src/inference/options.rs::resolve_spotter -->

## Hear about new versions: `[updates]`

```toml
[updates]
check = true
```
<!-- src: src/config/user_config/updates.rs::UpdatesConfig -->

Turn this on if you’d like the terminal and web clients to tell you when a new version is
available. The host checks efference.net at startup and about once a day afterwards. It
never installs an update for you; run the install command again when you’re ready.

Setup asks about this with yes preselected and saves your answer. You can change it through
setup’s “Update check” entry or by editing `check`. With no saved answer, no automatic
check happens. To check just once, run `efference update --check`, regardless of this
setting. [How efference runs](how-it-runs.md) lists what the request sends.
<!-- src: src/bin/efference/serve.rs::host (UpdatesConfig::load gates check_for_updates); src/host/update_check.rs::check_for_updates; src/setup/updates.rs::ask, ask_unless_answered; src/setup/manage.rs::run (Update check entry); src/bin/efference/update.rs; src/clients/tui/render_status_bar.rs; web/src/ui/ComposerToolbar.tsx::NewerVersionLine -->

## Make the terminal your own: `[tui]`

```toml
[tui]
theme = "Nord"
images = "kitty"
```
<!-- src: src/config/user_config/tui.rs::TuiConfig; src/config/user_config/tui.rs::ImageMode -->

- **`theme`**: a built-in theme's name, or the path to a `.tmTheme` file. An unknown name
  lists the built-in ones when the terminal interface opens. The theme picker in the
  terminal interface changes the theme until you quit; to keep one, set it here. The
  default is `"Catppuccin Frappe"`.
- **`images`**: how images in tool results are drawn. `"auto"`, the default, asks the
  terminal. Inside tmux, which hides the terminal from that question, `"kitty"` or
  `"iterm2"` names the terminal's image protocol. `"off"` shows each image as a one-line
  reference.
<!-- src: src/config/user_config/tui.rs::TuiConfig (theme, expand_home); src/clients/tui/keyboard.rs (ThemePicker: Enter closes the picker, writes nothing); src/config/user_config/tui.rs::ImageMode -->

## Editor support

`efference setup` starts a new file with a `#:schema` line. An editor with a TOML language
server — Taplo, which Even Better TOML uses, or Tombi — then completes keys, describes each
on hover, and marks a misspelled one. The editor downloads the description from
efference.net, so efference.net sees a request from your machine when you edit the file;
deleting the line stops that, and efference itself does not read it.
<!-- src: src/setup/render.rs::render (schema line on a fresh file); src/config/config_schemas.rs; assets/schemas/user.v1.json -->

## Configuration reference

### Every key

| Key | What it sets | Written by |
| --- | --- | --- |
| `[connections.<name>]` `provider` | A provider efference knows: `anthropic`, `openrouter`, `openai`, `openai-codex` (a ChatGPT subscription), or `typesafe` (spotter models only) | setup |
| `dialect`, `base-url` | Instead of `provider`, an endpoint's API (`messages`, `responses`, `completions`, or `systemone`) and its address | setup |
| `auth` | Where the key comes from ([Credentials](credentials.md)). `name` on a `keychain` or `oauth` source sets the stored item's name; it defaults to the connection's. | setup |
| `disabled` | Keep the connection without using it | setup |
| `spotter-model` | The spotter model for sessions whose main model is on this connection | by hand |
| `output-cap-field` | On a `completions` endpoint, the request field that carries the output limit: `max_tokens` (the default) or `max_completion_tokens` | by hand |
| `body` | On a `completions` endpoint, fields added to every request body, such as `{ parallel_tool_calls = false }`. A field efference writes itself is refused. | by hand |
| `[defaults] spotter` | The spotter model for sessions that name none | setup |
| `env_hook` | The command whose output is a session's starting environment | by hand |
| `[secrets]` `get`, `set`, `delete`, `absent_status` | Commands that keep efference's secrets instead of the OS keychain | by hand |
| `[host]` `bind`, `public-url`, `tls-cert`, `tls-key` | The web client's address and certificate | by hand |
| `[updates]` `check` | Whether a host checks efference.net for a newer version once a day | setup |
| `[tui]` `theme`, `images` | How the terminal interface draws | by hand |
| `[tui]` `frame-cost` | Time each frame and print a summary on exit, for developing efference | by hand |
<!-- src: src/config/user_config.rs::InferenceConfig, ConnectionConfig, AuthConfig, Defaults; src/inference/api/provider.rs::Provider::NAMED; src/inference/api/provider/dialect.rs::WireDialect::NAMED; src/inference.rs (body and output-cap-field refused off completions); src/inference/api/request_options.rs::RequestOptions::new; src/platform/secrets/commands.rs::SecretCommands; src/config/user_config/host.rs::HostConfig; src/config/user_config/updates.rs::UpdatesConfig; src/config/user_config/tui.rs::TuiConfig; src/setup/render.rs::render, apply_change -->

`efference setup` creates the file readable only by you, and it holds no keys
([Credentials](credentials.md)). `efference setup --config <path>` writes another file.
<!-- src: src/setup/render.rs::write (0600 on create); src/bin/efference/setup.rs (--config) -->

### `env_hook`, precisely

The hook runs when a session starts without an environment from its client: a creation
from the web client or from a script that sends none, a resume, and a host's reopening of
open sessions at start. A session created by another session carries its creator's
environment, and so does one created from a terminal. For a session that works in its own
git worktree, the hook runs in the project directory, not the worktree.
<!-- src: src/host/create.rs (supplied_env); src/host/resume.rs::prepare_resume; src/host/boot_resume.rs; src/clients/control_tools/create.rs (Executor::Host: caller.child_env()); src/clients/control_tools/hand_off.rs; src/host/create.rs (establish_environment at directories.project()) -->

It runs directly, not through a shell, with the host's environment, no input, and its error
output discarded. Its output replaces the host's environment rather than adding to it: a
variable the host had and the hook did not print is absent from the session. The project's
`[env]` settings apply afterwards, and the variables connections read their keys from are
still removed from the agent's commands ([Credentials](credentials.md)).
<!-- src: src/config/env_table/launch.rs::run_hook (stdin null, stderr null, host_env applied); src/host/boot/env.rs::build_env_policy; src/config/env_table/child.rs::ChildEnv::apply -->

The hook has 30 seconds, and its output may be at most 1 MiB. Past either limit, or if it
exits with a nonzero status or prints something that is not an environment, the session
does not start, and nothing falls back to the host's environment. The last record must end
with a NUL byte, since output that stops partway through a record is what a cut-off write
looks like. A hook that fails or runs out of time is stopped together with the programs
it started. An empty list, `env_hook = []`, is refused rather than read as "no hook".
<!-- src: src/config/env_table/launch.rs::HOOK_TIMEOUT, HOOK_OUTPUT_LIMIT, run_hook, LaunchError::Unterminated, ProcessGroup (killpg on every path but a clean success); src/config/user_config.rs::InferenceConfig::from_toml (empty argv) -->
