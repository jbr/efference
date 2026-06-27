# Tools

## Design boundary: in-process over MCP subprocesses

Tools run in-process behind a `Provider` enum aggregated by a `Toolbox`, not
as MCP subprocesses. The reason is iteration velocity: an in-process call pays
neither JSON-RPC round-trip overhead nor process lifecycle management. A new
backend is a single enum variant; dispatch stays static with no `async-trait`
boxing.

## The `Tool` trait and progressive reveal

The `Tool` trait (realized as the `Provider` enum in `src/tools.rs`) carries
the metadata progressive reveal needs: name, input schema, and when-relevant
hints. Whether a tool definition appears in the preamble (hot, always present)
or surfaces later in the session (cold, behind a discovery hint) is a property
of how `src/project.rs` chooses which definitions to emit — not a separate
subsystem. Mid-session reveals land at cache breakpoints so they do not
invalidate the established prefix (see [substrate.md](substrate.md) on the
cache-prefix invariant). The real cost to watch is discoverability failure: a
capability that never fires because its hint did not trigger. Hint quality is
load-bearing.

The redaction policy — deterministic rule vs. fork vs. eventual small
distiller — is kept ruthlessly pluggable behind a trait. It is the most-iterated
knob; see `redaction-fork.md`.

## Tool taxonomy

Not everything belongs behind the in-process trait.

**Built-in (in-process):** the generic, language-agnostic primitives — `fs`
(read / list / search / write) and structured edit (`preview_edit` /
`retarget_edit` / `persist_edit`). These earn the typed hook: gating, staleness
checks, and line-numbered rendering for the fork all want direct access to the
result structure.

**Per-language tooling → bash or MCP, not built-in.** `cargo`, `rustc` /
`ferritin`, test runners, and similar language-specific tools are better shelled
out via `bash` or mounted over MCP than baked into the harness. This takes
cargo-mcp off the critical path — it becomes a later optional MCP mount, not a
blocker.

**`CLAUDE.md` / configurable frontmatter (skills): deferred.** "Read this
file" covers the need until there is a concrete reason for richer
progressive-reveal config.

## MCP integration: in-process reuse of the generated `Tools` enum

fs-mcp and semantic-edit-mcp are integrated in-process. The open question of
whether to reuse the generated `mcplease` `Tools` enum or re-wrap the
underlying state methods resolved toward reuse: the tool logic lives on the
per-tool param structs' `execute`, so the enum is the minimal seam.

Each crate gets one additive `lib.rs` (the binary and tests are untouched,
fully reversible) that exposes its modules, `Tools` enum, state struct, and an
`embedded(cwd)` constructor.

`McpServer<State, ToolEnum>` in `src/tools.rs` is generic over the two
instantiations (fs-mcp and semantic-edit-mcp). It holds state in a `RefCell`
— the agent loop is single-threaded and `mcplease`'s `execute` is synchronous,
so the borrow never spans an `.await`. Dispatching a call wraps the name and
arguments as `{"name", "arguments"}`, deserializes to the typed `ToolEnum` via
`serde_json::from_value`, and calls `Tool::execute` against the shared state.
Any failure (bad input or tool error) becomes an `is_error` result so the
`tool_use` always gets exactly one paired `tool_result` — the API rejects
orphans.

### Schema translation

`McpServer::from_state` translates `mcplease`'s `ToolSchema` (MCP
`inputSchema`, camelCase) into the Anthropic `tools[]` shape (`input_schema`,
snake_case) via `anthropic_def`. `serde_json::to_value` normalizes object keys
to sorted order, so the resulting definitions are byte-deterministic for the
session and safe for the tools cache tier (see `transport.md` on caching).
Definitions are computed once at mount time and reused as the wire `tools[]`.

### Shared-store side effect: resolved with `embedded`

The crates' default `new` persists to a shared on-disk store
(`~/.ai-tools/...`) that other servers read; mounting with the default
constructor would clobber it. `embedded(cwd)` instead builds the session store
in memory (`SessionStore::new(None)` — no disk, no file-watcher) and seeds the
working directory, so embedding touches no shared state and makes end-to-end
tests hermetic.

### Duplicate tool name rejection: filter and guard

Both servers expose `set_working_directory`. Advertising it would put a
duplicate name on the wire — the API rejects duplicate tool names with a 400.
`set_working_directory` is therefore listed in the `hidden` filter passed to
`McpServer::from_state` for both servers; efference seeds the working directory
itself at mount time. Additionally, fs-mcp's `move` and `delete` are filtered:
they earn nothing over `bash`'s `mv`/`rm` and are not worth the tool-doc
tokens.

A `mounted_toolbox_advertises_unique_tool_names` test in `src/tools.rs` guards
against future collisions as new servers or tools are added.

## Open items

**cargo-mcp** is not yet integrated — there is a `mcplease` version skew
between cargo-mcp and the other crates, and it shells out to real `cargo` /
`rustup`, which adds complexity. It remains off the critical path per the
taxonomy above.

**Forwarding, renaming, and subsetting MCP tools** (the progressive-reveal
dimension of which of a server's tools reach the wire) is unsettled. Tools are
currently exposed as-is after the hidden filter; per-session or per-turn
subsetting is tracked in the backlog.
