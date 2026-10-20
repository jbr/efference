# Permissions

## What you need to know

Every tool call the agent makes is decided before it runs, in three steps:

1. **Your project's settings.** `efference.toml` lists what may run without asking: tools
   by name, files and directories, and shell commands by prefix. A call they cover runs.
2. **The spotter.** A call they don't cover goes to a second model, the spotter, which
   reads the call against the project's `scope` — a sentence saying what sessions here are
   for — and either lets it run or holds it.
3. **You.** A held call waits at a **gate** until you decide. There is no timeout.
<!-- src: src/agent/step.rs::run_call; src/agent/step/judge.rs::judge; src/policy.rs::classify, Verdict; src/spotter.rs -->

At a gate you can approve the call (**approve** in the terminal, **Allow** in the web
client), **decline** it with a message — the agent doesn't run it and carries on with what
you said — or **stop the turn**. In the terminal interface, typing a message and pressing
Return declines with it. The agent never learns that you approved something, only what
you declined and why.
<!-- src: src/frontend.rs::Approval; src/tui/control_strip.rs::strip_context_verbs; src/tui/state/gate.rs::submit_composer; web/src/ui/Gate.tsx::GateControls; src/agent/results.rs::rejected, aborted -->

These checks are built to catch an agent's mistakes. They are not a defense against an agent
that has been deliberately misled, by text in a file or a web page, or that is trying to get
around them: a permitted command runs with your full access, the spotter is a model that can
be wrong, and path rules compare text. When a project handles content you don't trust, grant
less and read each held call.
<!-- src: docs/permissions.md ("Threat model: error, not deception"), checked against src/spotter.rs (module doc) and src/policy.rs -->

### A starting point

```toml
scope = "Work on this Rust crate: edit its source and tests, run its build and tests."
watch-for = ["changes to CI configuration", "new dependencies"]
tools = ["efference.*"]

[paths]
"." = { read = true }       # read anything in the project; writes go to the spotter

[commands]
"cargo build" = true
"cargo test" = true
"cargo check" = true
"git status" = true
"git diff" = true
"git log" = true
"git push" = "deny"         # skip the spotter; always ask
```
<!-- src: src/config/raw.rs; src/policy/paths.rs::governing_grant; src/policy/commands.rs::command_stance; src/spotter.rs::render_context (watch-for) -->

This lets the agent read the project and build and test it without asking. Every file
edit goes to the spotter, which judges it against `scope` and `watch-for`. Granting writes
outright is possible — `"." = true` — but then no edit is looked at by anything. That
includes edits to files that run later as you: `efference.toml` itself, git hooks and
config, build scripts, and CI definitions.
<!-- src: src/policy.rs::classify_structured (a cleared write never reaches the spotter); src/config/load.rs (efference.toml read at session start) -->

The setup session efference offers in a new project writes this file with you
([Quick start](quick-start.md)). A session started in a directory with no `efference.toml`
— the setup session, or any other — may read and write that directory and use efference's
own tools; everything else goes to the spotter.
<!-- src: src/boot/scaffold.rs::CONFIG_SCAFFOLD, snapshot_config -->

### What a permission covers

A command permission is trust in a program, not a check on what it does. `"cargo test"`
runs your build scripts and tests; `cargo = true` also allows `cargo install`, which
downloads and builds code. A permitted command's arguments aren't checked against
`[paths]`: with `cat = true`, `cat /etc/hosts` runs.
<!-- src: src/policy/commands.rs::command_stance, classify_bash (no argument-path check) -->

Some programs run other programs, or write files, given only an argument: shells and
interpreters (`bash`, `python`, `node`), `env`, `xargs`, `sudo`, `ssh`, `docker`, `make`,
`find`, `sed`, and tools with a `-c` or `--config` option, such as `git` and `cargo`.
Permitting one by name permits whatever the agent can make it do. Permit the narrowest
prefix that covers the work: `"git log"`, not `git`.
<!-- src: src/policy/commands.rs::command_stance (prefix only) -->

Don't permit `efference`, `curl`, or an interpreter in a project whose agent you don't
want reaching the host: the host's control socket doesn't tell the agent apart from you, so
a program that can reach it can approve the agent's own held calls.
<!-- src: src/daemon/handler.rs::Transport::Socket (no gate); src/control_tools/host_executor.rs (self-decision rule applies to the agent's own tools only) -->
<!-- CHECK: not run end to end. Raise with jbr: is this user education, or a socket-side rule (a session's child processes cannot decide its gates)? -->

efference is not a sandbox. Everything the agent runs, runs as you, with your files and your
network. Nothing restricts which hosts a command can reach. A command you allow once can
also leave something running — a server, a watcher — after it returns.
<!-- src: absence of sandboxing in src/; src/bash.rs; src/policy/commands.rs (no network rule) -->

`[paths]` and `[commands]` don't apply to MCP tools. Permitting a tool by name, or a whole
server with `server.*`, trusts it with everything it can do, including tools the server adds
later.
<!-- src: src/policy.rs::classify_structured, tool_granted -->

### Saying no

`"deny"` in `[commands]` skips the spotter and always comes to you; you can still approve
the call. (An autonomous session, with no one to ask, declines it.) It matches a command
that starts with exactly those words. It does not match the same action written another
way — `git -C dir push`, `git add . && git push`, or a script that pushes — which the
spotter judges instead.
<!-- src: src/agent/step/judge.rs::judge (Verdict::Deny arm); src/policy/commands.rs::command_stance, classify_bash -->

`false` in `[paths]` removes access under that path, so a call that needs it goes to the
spotter. The spotter is shown the restriction and will normally hold the call, but it is a
model, not a lock. To keep a file out of the agent's reach, keep it outside the project and
out of the environment efference runs in.
<!-- src: src/policy.rs::classify_structured; src/policy/paths.rs::path_granted_at; src/spotter.rs::render_context -->

### When you're away

If you've set up the web client on your phone ([Remote access](remote-access.md)) and
turned on notifications, a gate sends one. It shows the tool and what the agent *says* it
wants, not the command itself, so open the gate and read the call before approving. While
you are actively using a client, the notification waits two minutes and is dropped if you
look at or decide the gate first. A session another session supervises notifies you only
about gates that come to you: one passed up to you, or one left with you when its
supervisor ends or you take the session back.
<!-- src: src/daemon/push.rs::push, MomentKind::title, gate_body; src/daemon/delayed_push.rs::PUSH_DELAY, cancel_delayed_push; src/daemon/attention.rs; docs/web-push.md (routing, not_sent), checked against src/daemon/approvals.rs::escalate -->

In the terminal interface, pressing Return with nothing typed approves a gate.
<!-- src: src/tui/state/gate.rs::submit_composer -->

## How this works, precisely

### The order of checks

For each call:

1. A question the agent asks you (`ask`) is not checked; it waits for your answer.
2. An edit to the session's own configuration file through efference's file tools, and a
   hand-off to a new session, skip steps 3 and 4 and go to whoever decides this session's
   gates.
3. Your settings: the call runs, goes to the spotter, or (`deny`) skips to step 5.
4. The spotter: runs it, or holds it.
5. Whoever decides this session's gates — you, or a supervising session — decides.
<!-- src: src/agent/step.rs::run_call (ask, config_write, hand_off); src/agent/step/judge.rs::judge; src/policy.rs::is_config_write -->
<!-- TODO: blocked on backlog/inbox/config-write-by-cleared-command.md — a permitted shell command that rewrites the session config is adopted without step 2. -->

Step 3 costs nothing and asks no model. Decisions from the spotter and from people are
recorded in the session's log.
<!-- src: src/policy.rs::classify; src/agent/step/judge.rs::judge (Payload::Spot); src/daemon/approvals.rs -->

### What clears without asking

- **`tools`**: efference's own tools and MCP tools, by name. `efference.*` is every tool
  efference provides; `server.*` is all of one MCP server's tools.
- **`[paths]`**: for efference's file tools, read or write access by directory. A file tool
  needs both its name in `tools` and access to every path it names.
- **`[context]`**: files listed there for the session to start with are readable by the file
  tools without a `tools` or `[paths]` entry, and writable if the entry says so.
- **`[commands]`**: shell commands by prefix, each `true` (or `"clear"`), `"deny"` (or
  `false`), or `"gate"`/`"watch"` (send to the spotter even under a broader `true`).
- The session's scratch directory (`$TMPDIR`) is always readable and writable.
<!-- src: src/policy.rs::tool_granted, classify_structured; src/policy/paths.rs::path_spec, context_granted, governing_grant; src/policy/commands.rs::command_stance; src/config.rs::CommandStance; src/boot/scaffold.rs::mint_scratch -->

### Shell commands

efference parses each command with a bash parser. Only a single plain command can be
cleared by `[commands]`. A pipe, `;`, `&&`, a subshell, `$(…)`, a `$VAR`, a glob, or a
redirection to a file sends it to the spotter instead, whatever is permitted. Three shapes
are allowed: `2>&1`-style descriptor redirection; one leading `cd <dir> &&` into a
directory `[paths]` lets the agent read; and, for a command that is itself permitted, one
`>` or `>>` write — with or without a here-doc — to a file `[paths]` lets it write.
<!-- src: src/policy/commands.rs::single_and_or_list, simple_command_tokens, is_safe_fd_dup, bash_tokens_in, classify_redirect_write, redirect_write_in, literal_heredoc -->

Prefixes match whole words from the start, as written: `"git diff"` matches
`git diff HEAD~1` but not `git -C dir diff`. The longest matching prefix decides, so a
narrow entry can carve an exception out of a broad one.
<!-- src: src/policy/commands.rs::command_stance -->

### Files

Paths are compared as text, without following symbolic links, so `[paths]` is a guard
against mistakes, not access control. Underneath your settings, the project's `.efference/`
directory — session records and configuration — has no access, except that a session may
read its own configuration and attachments; a deeper entry of your own can override that.
`~/.efference/` and paths outside the project have no special protection: they are
ungranted, so a call reaching them goes to the spotter unless something you permitted
covers it.
<!-- src: src/policy/paths.rs::lexical_normalize, path_granted; src/boot/scaffold.rs::self_surface_grants; absence of a ~/.efference rule in src/policy/ -->

### The spotter

The spotter sees the call (for efference's file tools and MCP tools, a description of the
tool and its arguments; for shell commands, the command and the paths it names), the
project's `scope` and `watch-for`, the working directory, and your `[paths]` settings. It
does not see the conversation, or the reason the agent gave for the call. An MCP server's
own claims about its tools are grounds to hold, never to run. The spotter can let a call run
or hold it; it cannot refuse one. If it fails or gives no usable answer, the call is held.
<!-- src: src/spotter.rs::render_context, render_move, render_brief, parse_spot, Spotting::fail_closed; src/policy/path_words.rs -->

Each call it judges is a model request on the spotter's own connection, so the command
text, scope, and path settings go to that provider. Its model is the first of: the "spotter
model" chosen for the session; `[defaults] spotter` in `~/.efference/config.toml`; the
connection's own `spotter-model`; a decision model built for this, when you have a TypeSafe
or OpenRouter connection; or a small model from the main provider. Adding an OpenRouter
connection can therefore change which provider the spotter's requests go to. A decision
model also compares the call with earlier calls in the session that you approved.
<!-- src: src/inference/options.rs::resolve_spotter, decision_spotter; src/api/provider.rs::default_spotter_model; src/agent/step/judge.rs::judge (RequestOrigin::Spot); src/spotter/precedent.rs -->

When the spotter sees a command prefix more than once, it may propose permitting it in the
session. A proposal changes nothing until you accept it, in the terminal interface's sidebar
or with `efference control proposal`.
<!-- src: src/policy.rs::MIN_SIGHTINGS_TO_PROBE; src/agent/step/judge.rs::judge (probe); src/tui/sidebar.rs (Proposal); src/control_tools (proposal) -->

### Grants made at a gate

A held call can come with one lasting permission that would have let it run, chosen from
why it was held: the tool's name, access to the directory the call reached (read-only when
the call only reads), or for a shell command the shortest prefix of it the spotter has not
judged unsafe — often the program's name alone, which permits it with any arguments. A call held for more than one reason, or one
no single permission would clear, comes with none. Neither the terminal interface nor the
web client offers this permission yet.
<!-- src: src/policy/gate_grant.rs::gate_grant; src/agent/config_edit.rs::commit_config_edit; src/tui/control_strip.rs::strip_context_verbs; web/src/ui/Gate.tsx -->
<!-- TODO: blocked on backlog/gate-grant-clients.md — restate as the clients' control once both offer it. -->

A permission granted this way is written to the session's own configuration in
`.efference/<session>/config.toml`, not to your project's `efference.toml`. A resumed
session keeps it; a new session starts from `efference.toml` again, so copy the ones you want
to keep into it.
<!-- src: src/agent/config_edit.rs::commit_config_edit; src/boot/scaffold.rs::snapshot_config -->
<!-- TODO: an accepted spotter proposal is logged, not written to config.toml (src/daemon/config_edits.rs::resolve_proposal_decision) — asked of jbr. -->

The agent can ask for a permission by editing its session configuration itself. That edit
goes straight to whoever decides the session's gates. Read the change itself: a new `scope`
sentence changes what the spotter allows, and a whole-file rewrite can change more than the
line it points to. Once approved, added permissions take effect at once. Removed ones, and a
new `deny`, take effect when the session next starts.
<!-- src: src/agent/step.rs::run_call (config_write); src/daemon/watch.rs::config_watch_loop; src/config/edit.rs::diff_changes -->

### Who decides

A session's review level sets who answers a held call:

| Level | Spotter | Held calls go to |
| --- | --- | --- |
| `spotter_human` (the default) | yes | you |
| `human_only` | no | you, for every call your settings don't cover |
| `supervisor` | yes | the session that supervises this one; the default for a session another session created. It can pass a call up to its own supervisor, and so on up to you. With no supervisor running, you. |
| `autonomous` | yes | no one: the call is declined, and the agent is told what to change in its configuration |
<!-- src: src/policy.rs::ReviewLevel; src/daemon/config_edits.rs::set_autonomous; src/daemon/host/supervise.rs; src/control_tools/decide.rs (escalate); src/daemon/approvals.rs::escalate; src/daemon/review.rs::resolved_review_level; src/agent/config_edit.rs::missing_grant_message, autonomous_gate_message -->

`efference control create --autonomous` starts a session that way; `efference control review-level` changes it. An
autonomous session runs whatever your settings or the spotter allow, with no one watching.
It still holds edits to its own configuration for a person.
<!-- src: src/control_tools/create.rs::Create::autonomous; src/control_tools/review_level.rs; src/agent/step.rs::run_call -->

Separately, `efference control pace` pauses a session between turns, or slows it down,
without changing who decides.
<!-- src: src/control_tools/pace.rs::Pace -->

### Worktrees

A session started in its own worktree works in a separate git checkout and branch, under
`.efference/worktrees/`, so its edits don't land in the checkout you're using by default. It
is not a permission boundary. It carries the project's settings, so it can still reach your
checkout by path, and it shares the repository, its hooks, and everything else on the
machine.
<!-- src: src/worktree.rs; src/policy/directories.rs::Directories::target, judged -->
