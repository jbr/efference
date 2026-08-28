# The `[env]` table — stripping at spawn, masking at append

One name-keyed table in `efference.toml`. Each entry answers two independent
questions, asked at two different points in the session:

```toml
[env]
"*_KEY" = { mask = true, strip = false }   # a secret the tools still need
"EFFERENCE_*" = { strip = true }           # env hygiene, no masking
DEPLOY_TOKEN = { mask = true, strip = true }
```

- **`strip`** — answered at **subprocess spawn**: does the child's environment
  include this variable? It also serves determinism — a stable child env makes
  tool output reproducible.
- **`mask`** — answered at **log append**: does the log ever store this
  variable's value?

The two are a **2×2, not a scale**. Strip-without-mask is the hygiene cell
(`COLORTERM`, the harness's own self-vars): a scale would force masking onto
hygiene values and shred output that has nothing secret in it. Mask-without-strip
is the ordinary case for a credential a tool genuinely needs.

Keys are exact variable names or `*` globs (`*` matches any run of characters;
that is the whole syntax). Only the inline-table form parses — a bare
`"*_KEY" = false` cannot say which of the two questions it is answering, so it
is a parse error rather than a guess.

Implemented in `src/env_table.rs` (the table, precedence, the strip list) and
`src/env_table/mask.rs` (needles, the marker, the scan).

## Threat model: accidental capture

This guards **accidental capture** — sticky-note opsec, not evil-maid. A
deliberately exfiltrating model defeats any string matcher (`cut -c1-4`, a
base-N re-encode, an arithmetic transform); the boundary that stops that is a
real sandbox, not the harness. The same error-not-deception model
[permissions.md](permissions.md) states applies here.

It complements the spotter rather than duplicating it: **approval gates guard
the action direction** of an accident (a session about to do the wrong thing),
**masking guards the disclosure direction** (a session about to record something
it should not). Neither subsumes the other, and neither is adversary-grade.

### Known gaps

- **Values the harness process does not have are invisible.** A secret that
  lives only in a `.env` file, or that `direnv` injects into a subprocess
  without ever entering the daemon's environment, produces no needle. Naming a
  value *source* (`{ file = ".env" }`) is the conditional extension if that gap
  bites; the trigger is someone finding an unmasked secret in a log.
- **Signed thinking blocks are not masked.** The provider mints a signature over
  a thinking block's exact text and rejects a modified block on replay, so
  masking one would end the session rather than protect it. The realistic path
  into thinking is a tool result — which *is* masked — so the model rarely has
  the bytes to think about.
- **Image bytes are not scanned.** `ResultImage.data` is base64 of an encoded
  image; a substitution there corrupts the image and protects nothing.
- **The Codex strip gotcha.** A tool whose credential was stripped dies with an
  opaque auth error and the session has no way to connect the two. Today the
  mitigation is documentation: if a tool starts failing to authenticate, check
  whether `[env]` strips the variable it reads. Naming the stripped variable in
  the failure is the improvement this gap is waiting for.

## Masking

### The choke point is log append

Every string about to enter the log is scanned, at
`Log::append_submission` (`src/persist.rs`) — the one function both `append`
paths funnel through. That single point covers tool results, the boot
`Preamble` (including a `[context]` exec snapshot of the environment),
`Ambient` notifications, user paste, and harness-written error text, and because
every downstream surface reads the log, one choke point secures all of them: the
wire, the jsonl on disk, the TUI, and the `log`/`attach` views.

The payload paths it filters, exhaustively: `User` text blocks; `Assistant` text
blocks, tool-call `input` strings, and `upstream_failure`; `ToolResult`'s
`raw.content`; `Mark.note`; `Triage`'s `raw_keep` / `surfaced` / `retries` /
`thinking`; `Ambient`'s `source` / `body` / `slot`; `Spot`'s `reason` and
`probed_prefix`; `ConfigEdit`'s `note` and its change payload; `Preamble`'s
`system` and its resolved `[context]` listing.

One string is masked *before* the append as well as at it: the boot preamble.
It both rides the request's `system` field and is snapshotted into the log, and
the point of the `Preamble` entry is that those two are byte-identical
([context.md](context.md)) — so `serve` masks the resolved `[context]` listing
and the rendered `system` string, and the choke point's re-scan is then a no-op.

### It is not a `WireState`

`WireState` ([log-and-projection.md](log-and-projection.md)) is **reversible
curation over immutable raw**: a verdict is flipped, `raw` is never overwritten,
and any redaction can be undone. Masking is the inversion — **irreversible
filtering before the log**. The secret never becomes `raw`, because a raw copy
on disk is half of what masking exists to prevent. A future change that
"simplifies" masking into a `WireState` variant would reintroduce exactly the
artifact this avoids.

Byte-determinism of `project` is untouched: the filtering happens upstream, so
by the time projection reads an entry the stored text is already final.

### The marker

A masked value is replaced by a marker that names the variable and shows a
**fixed** four-character prefix of the value:

```
⟦ANTHROPIC_API_KEY: sk-a… elided by harness⟧
```

Fixed, never proportional — a proportional prefix leaks the value's length. The
same value always produces the same marker, which is what keeps the diagnostic
pattern working: the questions a session actually asks about a credential are
"is it set?" and "does this one match that one?", and both are answered by the
marker's existence and equality without any value bytes crossing. A bare
`[REDACTED]` answers neither, and a blocked diagnostic pushes a well-intentioned
session into working around it (`echo ${KEY:0:8}`, `md5sum`) — each workaround a
fresh leak. The marker should not regress to an opaque placeholder.

### The maskability floor

Values under **8 characters** are not masked, and the variable is named (name
only) in a startup warning. Without the floor, `SESSION_KEY=dev` shreds every
occurrence of "dev" in every tool result.

### Transforms

A secret rarely reaches a tool result verbatim, so one value contributes several
needles:

- **line-wise, for a multiline value** — the top real-world miss is a PEM key
  read back through `cat -n` / `grep` / `diff`, where each line carries a prefix
  and the whole blob never appears contiguously.
- **base64 at three phase offsets** — a value embedded in a larger base64 blob
  encodes differently depending on its byte offset mod 3, so all three
  alignments are needled (the characters that mix in neighbouring bytes' bits
  are trimmed off).
- **percent-encoded** and **JSON-escaped** — the two shapes a value takes when
  it rides a URL or a JSON body back out of a tool.

### Mechanics

Needles are scanned **longest first** (so nested secrets resolve to the longer
one) in a deterministic total order (length descending, then lexicographic), in
a **single left-to-right pass** over the assembled final string. The pass
advances past each emitted marker, which is what keeps a short needle from
matching inside one — a repeated `replace` could not promise that.

Values are snapshotted at startup and re-snapshotted on a config edit. The
needle set is **append-only for the session**: a variable an edit stops masking
keeps its needles, because a rotated-out secret is still a secret and still sits
in files the session may go on to read. There is no zeroize window before exit —
the values are needed for the entire runtime — so the guarantee is exactly
*never written to disk, always masked at append*, and stops there.

## Defaults and precedence

Built-in entries — `*_CREDENTIALS`, `*_KEY`, `*_PASSWORD`, `*_SECRET`,
`*_TOKEN` — carry `mask = true`. Default-on is right for a safety feature, and
being *table entries* rather than code keeps them visible and overridable.

An operator entry at a built-in's key **merges** with it: an omitted field
inherits. So `"*_KEY" = { strip = true }` composes with the inherited
`mask = true` rather than disabling it — the ladder (allowed → masked →
masked+stripped) is the natural path through the defaults, not a third axis.
Disabling is possible, but only by saying `mask = false`.

**Precedence** for one variable name:

1. An **exact-name** key beats every glob.
2. Among matching globs, **more literal (non-`*`) characters wins**.
3. An unresolvable tie — two globs with the same literal count — is a **startup
   config error** naming both keys and the variable. (A caller that cannot fail
   resolves the tie to the union of the two rules, so the fallback is closed
   rather than silently empty.)

Never file order: TOML tables are semantically unordered.

Efference's own upstream credential (`EFFERENCE_API_KEY`, `ANTHROPIC_API_KEY`)
is masked with zero config and independently of the table, so overriding the
`*_KEY` glob cannot silently unmask the key this process authenticates with.

`EFFERENCE_ROOT_SECRET` is **not** a table entry ([web-bind.md](web-bind.md)).
Its strip is hardcoded self-surface protection at the spawn sites, like the
session-dir path deny: with it, a tool child could derive the bearer and drive
its own approval gates. If it ever appears in an effective-table display for
visibility, it must not be overridable downward.

## The spawn sites

The strip set is fixed at boot and applied at every place the harness starts a
subprocess:

| Site | Module |
| --- | --- |
| the foreground `bash` tool | `src/bash.rs` |
| a backgrounded `bash` command | `src/daemon/background.rs` |
| a stdio MCP server | `src/mcp/stdio.rs` |
| a `[context]` `exec` entry at boot | `src/context.rs` |

At the MCP site the strip is applied *before* the stanza's own `env` map: a
server explicitly handed a variable in its own config is being given it
deliberately, and a blanket strip should not silently undo that. The machine
root is removed unconditionally at all four.

## Deliberately not built

- **`value = "…"` / file sources** — setting or overriding a child's environment
  from the config. That is the secret-references unit's shape (a literal or a
  vault reference); the table's `strip`/`mask` pair leaves room for it.
- **Shape-based detection of *unregistered* secrets** (Gitleaks/detect-secrets
  rules). If ever, it would be a flag raised to the human, never silent masking:
  a heuristic that shreds output the operator did not declare is worse than the
  leak it prevents.
