# Permissions

You don’t have to choose between approving every step and letting an agent do anything.
A project can allow routine work, ask a second model to review calls outside those
permissions, and bring held calls to you. The place to describe that balance is your
project’s `efference.toml`.
<!-- src: src/agent/step/judge.rs::judge; src/config/policy.rs::classify, Verdict; src/curation/spotter.rs -->

## Decide what needs your attention

For an ordinary tool call, efference checks three things in order. The
[detailed order](#the-order-of-checks) covers special cases such as questions and edits to
a session’s own configuration:

1. **Your project's settings.** `efference.toml` lists what may run without asking: tools
   by name, files and directories, and shell commands by prefix. A call they cover runs.
2. **The spotter.** A call they don't cover goes to a second model, the spotter, which
   reads the call against the project's `scope` — a sentence saying what sessions here are
   for — and either lets it run or holds it.
3. **You.** A held call waits at a **gate** until you decide. There is no timeout.
<!-- src: src/agent/step.rs::run_call; src/agent/step/judge.rs::judge; src/config/policy.rs::classify, Verdict; src/curation/spotter.rs -->

When a call reaches you, read the call itself, not just the agent’s description of what it
wants. Then you can:

- **Approve** it (**approve** in the terminal, **Allow** in the web client).
- **Decline with a message.** The call won’t run, and the agent continues with your
  explanation. In the terminal, type the message and press Return.
- **Stop the turn.**

In the terminal, pressing Return with an empty message approves the call. The agent isn’t
told about approvals; it sees only what you declined and why.
<!-- src: src/record/log/approval.rs::Approval; src/tui/control_strip.rs::strip_context_verbs; src/tui/state/gate.rs::submit_composer; web/src/ui/Gate.tsx::GateControls; src/agent/results.rs::rejected, aborted -->

These are checks for mistakes, not a sandbox or a defense against a deliberately misled
agent. Allowed commands run with your full access, the spotter can be wrong, and path
rules compare text rather than enforce filesystem isolation. If the project handles
content you don’t trust — including instructions hidden in files or web pages — grant less
and inspect held calls carefully.
<!-- src: docs/permissions.md ("Threat model: error, not deception"), checked against src/curation/spotter.rs (module doc) and src/config/policy.rs -->

## Start with routine work

For a Rust project, you might begin by allowing reads, builds, tests, and git inspection,
while sending edits to the spotter:

```toml
scope = "Work on this Rust crate: edit its source and tests, run its build and tests."
watch-for = ["changes to CI configuration", "new dependencies"]

[tools]
efference = { enabled = true, granted = true }   # efference's own tools, without asking

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
<!-- src: src/config/raw.rs; src/config/policy/paths.rs::governing_grant; src/config/policy/commands.rs::command_stance; src/curation/spotter.rs::render_context (watch-for) -->

Here, `scope` tells the spotter what the sessions are for, and `watch-for` names changes
that deserve attention. The agent can read, build, and test without interrupting you;
file edits go to the spotter for review.

You can allow writes outright with `"." = true`, but those edits then pass without review.
That includes files that can run code later: `efference.toml`, git hooks and config, build
scripts, and CI definitions. Choose this based on what you’re comfortable letting the
agent change, not just how many prompts you’d like to avoid.
<!-- src: src/config/policy.rs::classify_structured (a cleared write never reaches the spotter); src/config/load.rs (efference.toml read at session start) -->

The setup session efference offers in a new project writes this file with you
([Quick start](quick-start.md)). A session started in a directory with no `efference.toml`
— the setup session, or any other — may read and write that directory and use efference's
own tools; everything else goes to the spotter.
<!-- src: src/boot/scaffold.rs::CONFIG_SCAFFOLD, snapshot_config -->

## Grant only the access you intend

A command permission is trust in a program, not a check on what it does. `"cargo test"`
runs your build scripts and tests; `cargo = true` also allows `cargo install`, which
downloads and builds code. A permitted command's arguments aren't checked against
`[paths]`: with `cat = true`, `cat /etc/hosts` runs.
<!-- src: src/config/policy/commands.rs::command_stance, classify_bash (no argument-path check) -->

Some programs run other programs, or write files, given only an argument: shells and
interpreters (`bash`, `python`, `node`), `env`, `xargs`, `sudo`, `ssh`, `docker`, `make`,
`find`, `sed`, and tools with a `-c` or `--config` option, such as `git` and `cargo`.
Permitting one by name permits whatever the agent can make it do. Permit the narrowest
prefix that covers the work: `"git log"`, not `git`.
<!-- src: src/config/policy/commands.rs::command_stance (prefix only) -->

Don't permit `efference`, `curl`, or an interpreter in a project whose agent you don't
want reaching the host: the host's control socket doesn't tell the agent apart from you, so
a program that can reach it can approve the agent's own held calls.
<!-- src: src/daemon/handler.rs::Transport::Socket (no gate); src/control_tools/host_executor.rs (self-decision rule applies to the agent's own tools only) -->
<!-- CHECK: not run end to end. Raise with jbr: is this user education, or a socket-side rule (a session's child processes cannot decide its gates)? -->

efference is not a sandbox. Everything the agent runs, runs as you, with your files and your
network. Nothing restricts which hosts a command can reach. A command you allow once can
also leave something running — a server, a watcher — after it returns.
<!-- src: absence of sandboxing in src/; src/tools/bash.rs; src/config/policy/commands.rs (no network rule) -->

`[paths]` and `[commands]` don't apply to MCP tools. Granting a tool, or a whole server with
`server = { granted = true }`, trusts it with everything it can do, including tools the server
adds later.
<!-- src: src/config/policy.rs::classify_structured, tool_granted -->

### When you want a call held

`"deny"` in `[commands]` skips the spotter and always comes to you; you can still approve
the call. (An autonomous session, with no one to ask, declines it.) It matches a command
that starts with exactly those words. It does not match the same action written another
way — `git -C dir push`, `git add . && git push`, or a script that pushes — which the
spotter judges instead.
<!-- src: src/agent/step/judge.rs::judge (Verdict::Deny arm); src/config/policy/commands.rs::command_stance, classify_bash -->

`false` in `[paths]` removes access under that path, so a call that needs it goes to the
spotter. The spotter is shown the restriction and will normally hold the call, but it is a
model, not a lock. To keep a file out of the agent's reach, keep it outside the project and
out of the environment efference runs in.
<!-- src: src/config/policy.rs::classify_structured; src/config/policy/paths.rs::path_granted_at; src/curation/spotter.rs::render_context -->

## Review calls from your phone

With the web client on your phone and notifications enabled, you can step away and hear
when a call needs you ([Remote access](remote-access.md)). The notification shows the tool
and what the agent *says* it wants, not the command itself. Open the gate and read the call
before approving.

While you’re actively using a client, notifications wait two minutes. Looking at or
deciding the gate cancels the pending notification. For a session supervised by another
session, you hear only about gates that reach you: ones passed up to you, or left with you
when the supervisor ends or you take the session back.
<!-- src: src/session/push.rs::push, MomentKind::title, gate_body; src/session/delayed_push.rs::PUSH_DELAY, cancel_delayed_push; src/session/attention.rs; docs/web-push.md (routing, not_sent), checked against src/session/approvals.rs::escalate -->

## Permission rules in detail

### The order of checks

For each call:

1. A question the agent asks you (`ask`) is not checked; it waits for your answer.
2. An edit to the session's own configuration file through efference's file tools, and a
   hand-off to a new session, skip steps 3 and 4 and go to whoever decides this session's
   gates.
3. Your settings: the call runs, goes to the spotter, or (`deny`) skips to step 5.
4. The spotter: runs it, or holds it.
5. Whoever decides this session's gates — you, or a supervising session — decides.
<!-- src: src/agent/step.rs::run_call (ask, config_write, hand_off); src/agent/step/judge.rs::judge; src/config/policy.rs::is_config_write -->
<!-- TODO: blocked on backlog/inbox/config-write-by-cleared-command.md — a permitted shell command that rewrites the session config is adopted without step 2. -->

Step 3 costs nothing and asks no model. Decisions from the spotter and from people are
recorded in the session's log.
<!-- src: src/config/policy.rs::classify; src/agent/step/judge.rs::judge (Payload::Spot); src/session/approvals.rs -->

### What clears without asking

- **`[tools]`**: efference's own tools and MCP tools with `granted = true`. A bare key is a
  namespace: `efference` is every tool efference provides, `agents` the tools that reach
  other sessions, `lsp` the language-server tools, and a server's name all of its tools. A quoted key is one tool
  (`"agents.message"`), and overrides its namespace. The same table says which tools a
  session has at all: efference's own need `enabled = true`, so a tool efference adds later
  in a namespace of its own reaches no session that did not enable it.
- **`[paths]`**: for efference's file tools, read or write access by directory. A file tool
  needs both a grant in `[tools]` and access to every path it names.
- **`[context]`**: files listed there for the session to start with are readable by the file
  tools without a `[tools]` or `[paths]` entry, and writable if the entry says so.
- **`[commands]`**: shell commands by prefix, each `true` (or `"clear"`), `"deny"` (or
  `false`), or `"gate"`/`"watch"` (send to the spotter even under a broader `true`).
- The session's scratch directory (`$TMPDIR`) is always readable and writable.
<!-- src: src/config/policy.rs::tool_granted, classify_structured; src/config/tools.rs::ToolsTable, Config::tool_enabled; src/config/policy/paths.rs::path_spec, context_granted, governing_grant; src/config/policy/commands.rs::command_stance; src/config.rs::CommandStance; src/boot/scaffold.rs::mint_scratch -->

### Shell commands

Command permissions apply to a single plain command, checked with a bash parser. More
complex shell syntax goes to the spotter: pipes, `;`, `&&`, subshells, `$(…)`, variables
such as `$VAR`, globs, and file redirections. There are three exceptions that can still
clear without the spotter:

- Descriptor redirections such as `2>&1`.
- One leading `cd <dir> &&`, if `[paths]` allows reading that directory.
- One `>` or `>>` write to a file, with or without a here-doc, if the command itself is
  permitted and `[paths]` allows writing the file.
<!-- src: src/config/policy/commands.rs::single_and_or_list, simple_command_tokens, is_safe_fd_dup, bash_tokens_in, classify_redirect_write, redirect_write_in, literal_heredoc -->

Prefixes match whole words from the start, as written: `"git diff"` matches
`git diff HEAD~1` but not `git -C dir diff`. The longest matching prefix decides, so a
narrow entry can carve an exception out of a broad one.
<!-- src: src/config/policy/commands.rs::command_stance -->

### Files

Paths are compared as text, without following symbolic links, so `[paths]` is a guard
against mistakes, not access control. Underneath your settings, the project's `.efference/`
directory — session records and configuration — has no access, except that a session may
read its own configuration and attachments; a deeper entry of your own can override that.
`~/.efference/` and paths outside the project have no special protection: they are
ungranted, so a call reaching them goes to the spotter unless something you permitted
covers it.
<!-- src: src/config/policy/paths.rs::lexical_normalize, path_granted; src/boot/scaffold.rs::self_surface_grants; absence of a ~/.efference rule in src/config/policy/ -->

### The spotter

The spotter sees the call (for efference's file tools and MCP tools, a description of the
tool and its arguments; for shell commands, the command and the paths it names), the
project's `scope` and `watch-for`, the working directory, and your `[paths]` settings. It
does not see the conversation, or the reason the agent gave for the call. An MCP server's
own claims about its tools are grounds to hold, never to run. The spotter can let a call run
or hold it; it cannot refuse one. If it fails or gives no usable answer, the call is held.
<!-- src: src/curation/spotter.rs::render_context, render_move, render_brief, parse_spot, Spotting::fail_closed; src/config/policy/path_words.rs -->

Each review is a model request on the spotter’s connection, so that provider receives the
command text, scope, and path settings. To choose the model, efference uses the first
available option in this order:

1. The **spotter model** chosen for the session.
2. `[defaults] spotter` in `~/.efference/config.toml`.
3. The main connection’s `spotter-model`.
4. A purpose-built decision model, if you have a TypeSafe or OpenRouter connection.
5. A small model from the main provider.

Adding an OpenRouter connection can therefore change where spotter requests go. A decision
model also compares the call with earlier calls you approved in the session.
<!-- src: src/inference/options.rs::resolve_spotter, decision_spotter; src/inference/api/provider.rs::default_spotter_model; src/agent/step/judge.rs::judge (RequestOrigin::Spot); src/curation/spotter/precedent.rs -->

When the spotter sees a command prefix more than once, it may propose permitting it in the
session. A proposal changes nothing until you accept it, in the terminal interface's sidebar
or with `efference control proposal`.
<!-- src: src/config/policy.rs::MIN_SIGHTINGS_TO_PROBE; src/agent/step/judge.rs::judge (probe); src/tui/sidebar.rs (Proposal); src/control_tools (proposal) -->

### Changing permissions during a session

The agent can ask for a permission by editing its session configuration. That edit goes
straight to whoever decides the session’s gates. Read the change itself: changing `scope`
changes what the spotter allows, and a whole-file rewrite can change more than the line
the agent points to. Approved additions take effect at once; removals and new `deny`
entries take effect when the session next starts.
<!-- src: src/agent/step.rs::run_call (config_write); src/session/watch.rs::config_watch_loop; src/config/edit.rs::diff_changes -->

### Suggested permissions at a gate

Approving a call is not the same as granting lasting permission. Internally, a held call
can carry a suggested permission that would have cleared it, but neither the terminal nor
the web client offers that control yet.

The suggestion, when there is one, follows the reason the call was held: a tool name,
access to a directory (read-only for a read), or the shortest shell command prefix the
spotter hasn’t judged unsafe. That prefix is often just the program’s name, allowing any
arguments. Calls held for several reasons, or with no single permission that would clear
them, have no suggestion.
<!-- src: src/config/policy/gate_grant.rs::gate_grant; src/agent/config_edit.rs::commit_config_edit; src/tui/control_strip.rs::strip_context_verbs; web/src/ui/Gate.tsx -->
<!-- TODO: blocked on backlog/gate-grant-clients.md — restate as the clients' control once both offer it. -->

A lasting permission applied through this mechanism is written to the session’s own
`.efference/<session>/config.toml`, not the project’s `efference.toml`. Resuming keeps the
session copy; starting a new session copies the project file again. Put permissions you
want future sessions to have in the project file.
<!-- src: src/agent/config_edit.rs::commit_config_edit; src/boot/scaffold.rs::snapshot_config -->
<!-- TODO: an accepted spotter proposal is logged, not written to config.toml (src/session/config_edits.rs::resolve_proposal_decision) — asked of jbr. -->

### Choose who reviews held calls

A session's review level sets who answers a held call:

| Level | Spotter | Held calls go to |
| --- | --- | --- |
| `spotter_human` (the default) | yes | you |
| `human_only` | no | you, for every call your settings don't cover |
| `supervisor` | yes | the session that supervises this one; the default for a session another session created. It can pass a call up to its own supervisor, and so on up to you. With no supervisor running, you. |
| `autonomous` | yes | no one: the call is declined, and the agent is told what to change in its configuration |
<!-- src: src/config/policy.rs::ReviewLevel; src/session/config_edits.rs::set_autonomous; src/daemon/host/supervise.rs; src/control_tools/decide.rs (escalate); src/session/approvals.rs::escalate; src/session/review.rs::resolved_review_level; src/agent/config_edit.rs::missing_grant_message, autonomous_gate_message -->

`efference control create --autonomous` starts a session that way; `efference control review-level` changes it. An
autonomous session runs whatever your settings or the spotter allow, with no one watching.
It still holds edits to its own configuration for a person.
<!-- src: src/control_tools/create.rs::Create::autonomous; src/control_tools/review_level.rs; src/agent/step.rs::run_call -->

Separately, `efference control pace` pauses a session between turns, or slows it down,
without changing who decides.
<!-- src: src/control_tools/pace.rs::Pace -->

### Keep edits separate with a worktree

A session started in its own worktree works in a separate git checkout and branch, under
`.efference/worktrees/`, so its edits don't land in the checkout you're using by default. It
is not a permission boundary. It carries the project's settings, so it can still reach your
checkout by path, and it shares the repository, its hooks, and everything else on the
machine.
<!-- src: src/worktree.rs; src/config/policy/directories.rs::Directories::target, judged -->
