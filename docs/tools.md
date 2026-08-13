# Tools

## Design boundary: in-process for the built-ins

efference's own primitives run in-process behind a `Provider` enum aggregated
by a `Toolbox`, not as MCP subprocesses. The reason is iteration velocity: an
in-process call pays neither JSON-RPC round-trip overhead nor process
lifecycle management. A new backend is a single enum variant; dispatch stays
static with no `async-trait` boxing.

The boundary is *additive*, not exclusive: third-party servers mount as
**connected providers** (a `Provider` variant speaking the MCP wire — see
below) without dissolving the typed in-process hook the built-ins earn —
gating, staleness checks, and line-numbered fork rendering all want direct
access to the result structs.

## The `Tool` trait and progressive reveal

The `Tool` trait (realized as the `Provider` enum in `src/tools.rs`) carries
the metadata progressive reveal needs: name, input schema, and when-relevant
hints. Whether a tool definition appears in the preamble (hot, always present)
or surfaces later in the session (cold, behind a discovery hint) is a property
of how `src/project.rs` chooses which definitions to emit — not a separate
subsystem. Mid-session reveals land at cache breakpoints so they do not
invalidate the established prefix (see [log-and-projection.md](log-and-projection.md) on the
cache-prefix invariant). The real cost to watch is discoverability failure: a
capability that never fires because its hint did not trigger — hints deserve
the same iteration as the tools themselves.

The redaction policy — deterministic rule vs. fork vs. eventual small
distiller — is kept pluggable behind a trait. It is the most-iterated
knob; see `redaction-fork.md`.

## Tool taxonomy

Not everything belongs behind the in-process trait.

**Built-in (in-process):** the generic, language-agnostic primitives — `fs`
(read / list / search / write) and structured edit (`edit` /
`retarget_edit` / `undo_edit`). These earn the typed hook: gating, staleness
checks, and line-numbered rendering for the fork all want direct access to the
result structure.

**Per-language tooling → bash or MCP, not built-in — provisionally.** `cargo`,
`rustc` / `ferritin`, test runners, and similar language-specific tools are
shelled out via `bash` or mounted over MCP rather than baked into the harness.
This takes cargo-mcp off the critical path — it becomes a later optional MCP
mount, not a blocker. Held as the current resolution, not settled doctrine: the
longer-term position is that language support ("adding python support") wants a
deeper extension contract than MCP tool calls or LSP
(see [positions.md](positions.md)); nothing built so far commits against that.

**`CLAUDE.md` / configurable frontmatter (skills): deferred.** "Read this
file" covers the need until there is a concrete reason for richer
progressive-reveal config.

## MCP integration: in-process reuse of the generated `Tools` enum

fs-mcp and semantic-edit-mcp are integrated in-process. The open question of
whether to reuse the generated `mcplease` `Tools` enum or re-wrap the
underlying state methods resolved toward reuse: the tool logic lives on the
per-tool param structs' `execute`, so the enum is the smallest integration
surface.

Each crate gets one additive `lib.rs` (the binary and tests are untouched,
fully reversible) that exposes its modules, `Tools` enum, state struct, and an
`embedded(cwd)` constructor.

`McpServer<State, ToolEnum>` in `src/tools.rs` is generic over the two
instantiations (fs-mcp and semantic-edit-mcp). It holds state in a `RefCell`
— the agent loop is single-threaded and `mcplease`'s `Dispatch::call` is
synchronous, so the borrow never spans an `.await`. Dispatching a call wraps
the name and arguments as `{"name", "arguments"}`, deserializes to the typed
`ToolEnum` via `serde_json::from_value`, and calls `Dispatch::call` against the
shared state with a `RequestContext` naming efference and declaring no client
capabilities (no wire, so the context that would ride in a request's `_meta` is
handed over directly). The resulting `CallToolResult` is folded to text by the
same renderer the connected providers use, so an embedded tool that grows a
structured or multimodal result reads the same way as a remote one. Any failure
(bad input or tool error) becomes an `is_error` result so the `tool_use` always
gets exactly one paired `tool_result` — the API rejects orphans.

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

## Connected providers: user-declared MCP servers

Besides the embedded crates, efference mounts servers it did *not* compile in —
an arbitrary set the user declares in the config (`[mcp.servers.<name>]`,
`src/config.rs`), spawned over stdio or reached over streamable HTTP. The
implementation is `src/mcp.rs` (+ `src/mcp/stdio.rs`, `src/mcp/http.rs`);
`Provider::Connected` is the toolbox variant. The MCP `client`/`server` role
names describe wire direction only and stay inside that module; efference's
vocabulary is *host mounts provider*.

The stanza's shape picks the transport — `command`/`args`/`env` spawns a stdio
subprocess (own process group, killed on drop, stderr drained to the daemon
log), `url` connects over streamable HTTP (`trillium-client`; the server may
answer a POST as JSON or as a per-request SSE stream, and `Mcp-Session-Id` /
`MCP-Protocol-Version` ride as headers once learned). Declaring a server is the
human's authority via the reviewed config file; an agent write to that table is
gated like any other config edit, and *calling* a connected tool rides the same
spotter/gate cascade as every tool. A connected server's results are untrusted
content — same trust class as attachments; human-in-the-loop is the mitigation.

### The client protocol, and what a transport is left holding

Wire protocol and serialization types both live in `mcplease`, taken with the
`client` feature. `mcplease::client::ClientProtocol` builds each request —
including the per-request `_meta` (protocol version, client identity,
capabilities) that revision `2026-07-28` requires, and that a server on any
revision reconstructs as its `RequestContext` — and classifies each arriving
message as a `Reaction`: a result to correlate by id, a reply to send (`ping`
answered, everything else refused as method-not-found, since efference declares
no client capabilities), or nothing. `mcplease::client::Negotiation` runs the
`server/discover` probe `2026-07-28` introduced and falls back to the stateful
`initialize` handshake when a server answers that it has no such method —
which is what deployed servers do today. Neither type performs I/O.

That leaves `src/mcp.rs` one loop — `Session::round_trip`: send a request, read
frames until the response with a matching id arrives — shared by both
transports, and leaves the transports themselves as framing alone
(`src/mcp/stdio.rs` reads and writes lines; `src/mcp/http.rs` POSTs and exposes
whatever the server answered with, one JSON body or a stream of SSE events, as
a source of frames). Requests are strictly serialized — the loop issues one
tool call at a time — so there is no demux task. Mounting is negotiation, then
a paginated `tools/list`.

Unlike the embedded path, the tool table is **discovered at runtime**, which
creates a cache hazard the embedded path never had (the server can change its
list between — or during — sessions). The discipline: the table is a
**snapshot at mount**, sorted by tool name (a server may reorder), folded
through the same `anthropic_def` normalization, and frozen for the session; a
mid-session `tools/list_changed` does not reach the wire. Every failure — a
server that won't start is skipped with a warning at boot; a call that fails,
times out, or hits a dead server — lands as an `is_error` result, so a
`tool_use` is never orphaned. A `tools/call` result is decoded as
`ToolCallOutcome`, which discriminates on `resultType`: an *interim* result
(the server asking for elicitation or sampling, or shedding load and asking to
be retried) would otherwise deserialize as a complete result with empty
content. efference declares no client capabilities and does not re-issue, so an
interim result lands as an `is_error` result naming what the server asked for.

Both revisions are reachable: a server that answers `server/discover` is
mounted statelessly (no handshake, no `notifications/initialized`), and one
that does not gets the handshake. Loopback tests in `src/mcp/test.rs` cover
each path.

Tool names reach the wire namespaced `<server>__<tool>`. Two underscores are
legal in the API's tool-name alphabet, absent from every embedded tool name,
and the config validates server names against the same alphabet — so
collisions across N user-provided servers are structurally impossible rather
than guarded case-by-case.

Because `__` is what resolves the boundary in every direction, a server name may
not itself contain one — `_` is legal, `my__server` is rejected at validate.

The alphabet has no separator that reads as one to a human, so the TUI renders
the boundary as a dot — `ferritin__get` displays as `ferritin.get`
(`mcp::display_name`, applied at the two transcript sites that print a tool
name). The dotted form never reaches the wire or the log. It *is* accepted as a
config **input alias**, since an operator types what the TUI shows them:
`config::raw::wire_tool_name` resolves a dotted `tools` entry to the wire name at
validate, so a grant is still stored and matched as exactly one name. Only the
first dot is the boundary — a server's own tool name may contain dots (the MCP
spec permits them), so `ferritin.admin.tools.list` resolves to
`ferritin__admin.tools.list`.

Every human-facing surface uses the dotted form: the transcript, the sidebar's
policy labels (including pending proposals, so a proposal reads and sorts as the
grant it becomes), and the config file the auto-persist write-back produces.
The wire and the log keep the underscores — the dot is applied at each display
boundary and undone on the way back in, so exactly one name is ever stored or
matched.

Server-wide clearance (`tools = ["ferritin"]` clearing every tool that server
advertises) is not built:
[`backlog/permissions/tool-grant-granularity.md`](../backlog/permissions/tool-grant-granularity.md).

One known gap: the offline `wire` dev view does not mount connected providers
(mounting spawns processes), so a session with `[mcp.servers.*]` sends more
`tools[]` than that view shows.

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

## The session scratch dir and `$TMPDIR`

`serve` mints a private per-session scratch directory (`<system-temp>/efference-<rand>/`,
mkdtemp-style: random name, create-or-retry, mode `0700`) and projects that one value
three ways — the env var, the path grant, and the expansion value are all views of it:

- **`$TMPDIR` in every tool child.** The foreground `Bash` spawn and the supervisor's
  background spawn both override `TMPDIR` in the child environment. The ecosystem
  already respects it (`mktemp`, `std::env::temp_dir()`, Python `tempfile`), so
  incidental and deliberate temp files land in the sanctioned dir with no agent
  awareness required.
- **Always granted rw** — inserted into the path grants alongside the self-surface
  carve-out stack ([permissions.md](permissions.md)). It lives outside `.` and outside
  the session dir, so it collides with neither the operator's grants nor the deny. The
  spotter's scope block additionally renders the alias line (`$TMPDIR` → the literal
  path), because a *bash* move writes `$TMPDIR/…` and the shell expands in-child — the
  spotter sees the variable, and the alias is what connects it to the granted path.
- **Expanded in structured tool paths** (`src/expand.rs`). Bash gets the variable from
  the child env; `read`/`write`/`edit`/… don't go through a shell, so the loop expands
  `$NAME`/`${NAME}` in their *path fields only* (per the same `path_spec` key map the
  classifier uses — file content is never rewritten) at one choke point upstream of
  classify/spot/exec, so the fast-path, the spotter, and the executor all see the same
  expanded literal. Expansion is against a frozen boot-time map whose exposed set
  starts at exactly `{TMPDIR}`; an unlisted or undefined name is a hard error paired
  to the call as an error result — never expand-to-empty, never leave-verbatim
  (`$$` escapes a literal dollar). The grant check stays post-expansion: expansion
  resolves the name, `path_granted` resolves the permission on the literal.

The logged `tool_use` keeps the variable form the model wrote; only the in-flight call
is rewritten, so projection determinism is untouched. Lifecycle: deleted on session
end; `serve --keep-scratch` retains it as a post-mortem artifact.

## Open items

**cargo-mcp** is not embedded, and no longer needs to be: as a stdio binary it
is one `[mcp.servers.cargo]` stanza away (the connected path negotiates down
to its protocol revision; no `mcplease` version alignment required). It
remains off the critical path per the taxonomy above — mount it when driving
cargo through bash shows real friction.

**Forwarding, renaming, and subsetting MCP tools** (the progressive-reveal
dimension of which of a server's tools reach the wire) is unsettled. Tools are
currently exposed as-is after the hidden filter; per-session or per-turn
subsetting is tracked in the backlog.
