# Setting up a project for efference

Where a session's knowledge and perimeter are written down, and what to put there.

## Two files, two jobs

A session's policy and boot knowledge come from one TOML file. `efference serve` resolves
it in this order: `--config <path>` (which must exist), else `efference.toml` in the
working directory, else a scaffolded file with only an orientation entry in it. Nothing
else is scanned — there is no `EFFERENCE.md` convention, no auto-loaded `CLAUDE.md`, no
search for skill files.

At boot the resolved file is **copied once** into the session's own directory,
`.efference/<session-id>/config.toml`. That snapshot is the live policy for the rest of the
session; the original is an inert template, and two sessions started from one template
diverge freely. So:

- **`efference.toml` at the project root** — the template. Editing it changes what the
  *next* session starts with. This is the file a "set this project up" task writes.
- **`.efference/<id>/config.toml`** — this session's live perimeter. Editing it is how a
  running session *requests* a change: that write is always held for the operator, whatever
  else the config allows, and it takes effect the moment it is approved.

A malformed config is rejected whole rather than partially applied, and a rejected edit is
reverted, so if a change does not seem to take, check that the file still parses.

## A starter template

```toml
scope = "Work inside this repository: read, build, test, and edit its sources."

tools = ["read", "list", "search", "write", "edit", "find_anchor", "retarget_edit", "undo_edit"]

[commands]
cargo = true
"git status" = true
"git diff" = true
"git log" = true
"git push" = "deny"

[paths]
"." = true

[context]
"CLAUDE.md" = true
"Environment at boot" = { exec = 'echo "working directory: $PWD"; echo "platform: $(uname -sm)"' }
```

Every key is described in `efference://guide/config-format`.

## What belongs in `[context]`

`[context]` is the only route by which a session boots knowing anything about the project.
List the files every session should have:

- A bare path (`"CLAUDE.md" = true`) rides in the preamble **in full** — unless the file's
  own YAML frontmatter carries a `description`, in which case only a one-line stub rides
  and the body is read on demand through `read`.
- A directory expands to every `.md` file beneath it, recursively, one listing per file.
- Every listed path is read-granted automatically, so a stub's path resolves through `read`
  with no `[paths]` entry at all.

That last point cuts both ways: listing a directory read-grants everything under it. List
deliberately.

## What to leave out

- **Secrets.** Name a vault reference in `[env]` rather than a literal value; a resolved
  reference is masked out of the log automatically.
- **`watch-for`.** It is read only by the permission spotter and is never shown to the
  session, which is the point of it. It belongs to whoever sets the perimeter.
- **Anything the session should discover for itself.** The preamble is frozen at boot and
  every byte of it is re-sent on every request.

## Checking the result

Start a session and `read` its `.efference/<id>/config.toml` — it is a verbatim copy of the
template, comments and all. `efference state` prints the **effective** policy in force,
which is that file plus every change approved since boot.
