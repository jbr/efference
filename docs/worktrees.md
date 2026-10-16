# Worktree sessions

A session can work in its own git worktree: a second checkout of the project, on its own
branch, so its edits do not land in the checkout another session or the operator is
using. `POST /sessions {"worktree": true}` asks for one (`efference` CLI `--worktree`,
the `create` control tool's `worktree`). Worktree placement and every git operation live
in `src/worktree.rs`; the directory pair lives in `src/policy/directories.rs`.

## Project and working directory

Every session has two directories (`policy::Directories`, carried by `SessionPaths`,
`Session`, and the session header):

- the **project**: where `efference.toml` is read, where the session dir
  `.efference/<id>/` lives, what `[paths]` and `[context]` keys resolve against, and what
  the launch index and model history record;
- the **working directory**: where the session's tools run (bash, the built-in file
  tools, MCP mounts, background processes), what the paths a call names resolve against,
  and what a child session it creates inherits.

They are the same directory for a session without a worktree. A session is never run
*in* a worktree: there is one `.efference/` and one `efference.toml`, the project's, and
a worktree has neither of its own. The header records the working directory (`working`)
only when it differs, so a header without a worktree is unchanged.

The host derives the pair from the one `cwd` a creation request carries
(`worktree::project_of`): a `cwd` inside `<project>/.efference/worktrees/<id>` belongs to
that project and works where it was sent, so a session created from inside a worktree —
by a person in a terminal there, or by a session working there — inherits the worktree.
A child that wants its own asks for `worktree: true`. The user-wide `env_hook`
([env.md](env.md)) runs at the project, on creation as on resume, so a session's
environment does not depend on which of the two started it.

## Creation

With `worktree: true`, boot runs `git worktree add -b efference/<id>
<project>/.efference/worktrees/<id> <HEAD>`, from the creator's working directory and on
the commit its `HEAD` names, with the session's child environment. The worktrees sit
together under one directory rather than inside each session dir, so one path prefix
covers all of them. A project that is not a git work tree is refused before anything
is minted; a boot that fails after the add removes the worktree and its branch.

The checkout is a plain one, so the project's `post-checkout` hook runs. Warming a new
worktree — cloning build output, initializing submodules — is the project's concern,
expressed in that hook, not the harness's: what a warm tree needs is language- and
project-specific. (This repository's `.githooks/post-checkout` clones `target/` and
`web/node_modules` copy-on-write and initializes the theme submodules from the main
checkout.) Git returns the hook's exit status as its own, so a nonzero exit with the
directory present is a created worktree with a failed setup, not a failed creation.

Every git the harness runs drops git's repository-local variables (`GIT_DIR`,
`GIT_INDEX_FILE`, and the rest of `git rev-parse --local-env-vars`;
`worktree::unset_repository_env`). Git exports them to the hooks it runs, so an
environment captured inside one names that hook's repository; with them present, a
`git worktree add` acts on the wrong repository and a `git init` reinitializes it as
bare.

## The identity line

When the working directory differs from the project, the identity line
([context.md](context.md)) names both, and adds the branch when this session made the
worktree. Both are fixed at creation, so the sentence is byte-stable; a session without
a worktree renders exactly what it did before. A session never moves into or out of a
worktree mid-session for the same reason: the working directory is in the frozen
prefix.

## Permissions

`[paths]` keys resolve against the project and the paths a call names against the
working directory. A target inside any worktree, `.efference/worktrees/<id>/<rest>`, is
classified as `<project>/<rest>` (`worktree::counterpart`, applied by
`Directories::target`): every worktree, the session's own and every other session's,
carries exactly the project's grants, narrower ones included. A grant of `.` therefore
covers the project checkout and every worktree, for reading and writing — a worktree is
a convenience, not an isolation boundary, and path grants are mistake-guardrails
([permissions.md](permissions.md)). Only classification sees the counterpart; the tool
touches the path the call named. Both spotters are shown the same view: the project as
the working directory and each resolved path as its counterpart
(`Directories::judged_working`), because the worktree's real path sits under the
denied `.efference/` and a reviewer comparing strings read every in-tree move as a
denied path. `.efference/` itself is never mapped: it exists only at
the project, where the injected self-surface stack anchors.

A grant key that leaves the project (`../sibling`) resolves from the project, as its
author wrote it. A path the *model* types is resolved from its working directory and is
not rewritten: `../sibling` from a worktree names a directory inside
`.efference/worktrees/`, and the call fails visibly. The identity line's absolute paths
are what the model uses instead. Manifest paths that leave the repository (a Cargo path
dependency on `../sibling`) break the same way inside a worktree; absolute paths, or the
project's `post-checkout` hook, bridge them.

## Archive and resume

An archived session is resumable, so archive does not assume the session is finished
(`worktree::retire`, run from `Host::end` — archive only; a host shutdown leaves
worktrees in place):

- a worktree with no uncommitted or untracked changes (`git status --porcelain` empty;
  ignored files such as `target/` do not count) is removed; its branch is deleted only
  when another branch already contains its tip, and kept otherwise, since it then holds
  the only copy of those commits. A clean worktree on a detached `HEAD` no branch
  contains is kept;
- a worktree with changes is kept untouched.

The archive response's `worktree` field and a `Worktree` log entry say which happened
([control-api.md](control-api.md)). When the session never had a turn, a worktree that is
removed goes with a discarded session, and one that is kept makes it an ordinary archive
([resumption.md](resumption.md)). Resume re-adds a missing worktree the session made at
the same path — from its kept branch, or a fresh `efference/<id>` at the project's
`HEAD` — so the identity line is byte-identical and `post-checkout` warms it again
(`worktree::restore`). A session whose working directory was inside another session's
worktree, and finds it gone, is refused like any session whose working directory is gone.

Discarding a kept worktree is git's: `git worktree remove --force` and `git branch -D`.

## The log entry

`Payload::Worktree` (`src/log/worktree.rs`) records `added`, `restored` (git's base,
exit status, and output, which carries the project hook's), `removed` (whether the branch
went too), `kept` (why), and `remove_failed`. It projects to nothing, like `Cold`, and its
free text is masked like any other ([log-and-projection.md](log-and-projection.md)). It
is the audit trail for a slow, cold, or failed setup.

Both clients draw the entry as a one-line notice in their transcripts — the branch on an
add or restore with a failed setup's exit status on the line itself, and the outcome on
remove, keep, or failure ([tui.md](tui.md), [web-client.md](web-client.md)). The setup
`output` is not shown in the TUI; the web collapses it behind a disclosure.

## Git only

Placement, the permission rule, the identity line, the header, and the log entry are not
git-specific. Creating, removing, and the clean-and-merged check are, and they are the
whole of what `src/worktree.rs` runs. A non-git version control system (jj's
`jj workspace add`, Mercurial's `hg share`) would replace those three behind the same
functions.
