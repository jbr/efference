# Editing files

The file tools return structured results the harness can focus cleanly, which the shell
equivalents do not. This is what each one is for and how the edit pipeline behaves.

## Reading

`read` takes a `paths` array, so several files arrive in one call and one result:

```json
{"intent": "…", "paths": ["src/lib.rs", "src/main.rs"]}
```

`from_line` / `to_line` (1-indexed) and `max_length` (bytes) narrow a large file. Prefer
reading whole and letting the harness focus the result over slicing pre-emptively — a
narrow slice that missed the relevant lines costs a second call, and the focusing already
happened.

`search` finds a pattern across files and `list` gives gitignore-aware directory contents
(it skips `target/`, `.git/`, and anything else ignored, which `ls` and `find` do not).

## Writing whole files

`write` takes `path` and `contents`, and **refuses to clobber**: writing over a file that
exists requires `overwrite: true`, and that flag fails if the file does *not* exist. The
refusal comes back as an error carrying a retry token, so the correction is one call, not a
guess. `append: true` is the other mode and is mutually exclusive with `overwrite`.
Intermediate directories are created by default (`create_directories`).

Reach for `write` when the file is new or the change is most of it. For a change inside an
existing file, the edit pipeline below preserves everything you are not touching and tells
you what actually moved.

## The edit pipeline

Four tools over one piece of state: **the most recent edit**.

### `edit`

```json
{"intent": "…", "file_path": "src/lib.rs", "operation": "replace",
 "anchor": "fn greet(name: &str) {\n    println!(\"Hello, {name}\");\n}",
 "content": "fn greet(name: &str) {\n    println!(\"Hi, {name}\");\n}"}
```

- `operation` is `replace`, `insert_after`, or `insert_before`. For `replace`, omitting
  `content` **deletes** the anchored code.
- `anchor` is text copied out of the file — the whole item or statement you are operating
  on, not a fragment of it. Matching is whitespace-insensitive, so line breaks and
  indentation need not be reproduced exactly.
- The edit is validated before it is written: the result must still parse in the file's
  language and pass its formatter, or nothing is changed and the rejection comes back with
  the diff and the syntax errors. A valid edit is written to disk immediately.
- The whole file is run through the language's standard formatter, so the returned diff may
  include formatting fixes beyond your change.
- If the anchor matches more than once, the **first** match is edited and the result lists
  every match location. If it matches nowhere, or does not line up with complete syntax
  nodes, nothing changes.

### `find_anchor`

Same matching, no changes: it shows every location an anchor matches, with surrounding
context. Use it when you expect an anchor to be ambiguous — it is cheaper than an `edit`
that lands in the wrong place.

### `retarget_edit`

Takes an `operation` and a new `anchor` and moves the **most recent** edit there — reverting
it and re-applying its content in one step, without resending the content. This is the
answer when the diff shows the change landing somewhere other than intended. If it fails at
the new anchor, the file is left as it was, with the previous placement still applied and
still retargetable.

### `undo_edit`

Reverts the most recent edit, restoring the file exactly. Only one edit is held for undo —
each new edit replaces the undo state — and it fails without changing anything if the file
has been modified by something else since.

## Reading the result

An outcome that **refused** the request is flagged as an error: a `write` that would clobber,
an `edit` whose placement did not validate, a flag conflict. An outcome that answers the
call on its own terms is a success even if nothing changed: a `search` with no matches, an
`edit` whose exact change the file already contains. The flag reports whether the tool was
wielded correctly, not whether the world moved.

## When the shell is the right answer

`mv`, `rm`, and anything language-specific (`cargo`, formatters, test runners) go through
`bash`. The file tools cover reading, searching, listing, writing, and structured editing;
everything else is a command.
