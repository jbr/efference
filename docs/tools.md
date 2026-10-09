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

The redaction policy is the most-iterated knob; see `redaction-fork.md`. It is
one concrete type (`ForkPeek`), not a trait: the single-implementation
`Distiller` bound it was built behind was threaded through three signatures for
a polymorphism nothing exercised, and the swap the backlog names — a smaller
model behind the same peek — is a field and a different client, not a second
implementation.

## The `intent` property

Every tool the toolbox advertises carries one property the tool did not declare:
`intent`, a required string holding one line on why the call is being made and what
in the output would settle it. It is
injected once in `Toolbox::definitions` over the flattened set, so a built-in tool
and a connected server's tool get the same field on the same terms; the guidance for
writing it is in the session preamble (`assets/prompts/session.md`), and the schema
`description` is a pointer to that guidance.

The split is deliberate. The **declaration** has to be in each schema: a property the
schema does not declare may not be emittable at all under constrained decoding, and a
preamble-only instruction is a soft constraint that decays across a long mechanical
run — the regime where the field is most wanted. The **guidance** is prose stated
once, because repeating a paragraph across every mounted tool costs prefix tokens per
tool for no added signal.

`Toolbox::run` removes `intent` from `input` before dispatch (`Provider::without_intent`),
so no tool receives it. The logged `ToolCall` keeps it, which is what puts it on the
wire and in reach of `ToolCall::intent`. It stays inside `input` rather than becoming a
field of its own: the wire already carries it as part of the `tool_use` block, so
lifting it out would move the pinned projection bytes for no gain
([log-and-projection.md](log-and-projection.md)).

**Collision.** A tool whose own schema already declares `intent` is skipped — not
injected, and not stripped, since the field is the tool's own. The check reads the
mounted table, which is fixed for the session, so a name answers the same way at
definition time and at dispatch time; injection and strip cannot disagree. The
preamble says this happens, so a session meeting such a tool reads its description
rather than assuming the harness meaning.

**Determinism.** `serde_json::Map` is a `BTreeMap`, so the injected property lands in
sorted key position — an identical mounted set yields byte-identical definitions, which
the `tools[]` array sitting in the cached prefix requires. That sorted order is a
serde_json *cargo feature* (`preserve_order` swaps in an `IndexMap`), unified across the
whole runtime graph, so it is pinned by a test in `src/project.rs` rather than assumed;
see [log-and-projection.md](log-and-projection.md).

A consequence: the schema cannot put `intent` first, because key order is not the
schema's to choose (it falls after `command` for bash, before `paths` for `read`).
Whether the model *emits* it before the other arguments is the difference between a
reasoning slot and a label on a call already decided, and the levers for that are the
preamble instruction — the one being relied on — and prepending `intent` to `required`,
which is free but not a known mechanism, since `required` is semantically a set. Which
of these does anything is a reading to take off real calls, and the split sort position
across tools is a natural A/B.

**What the guidance asks for.** The preamble states the indirection as a fact about
how tools work here — the agent does not read tool output directly; a reader between
it and the tool keeps the part that answers this line — and frames `intent` as the
prompt to that reader: a question, plus what in the output would settle it. Every
prior a model carries says "I run the call and read the output", under which the
field reads as a caption, and a caption is what gets written for a reader that is
invisible; naming the reader is the lever, not asking for better captions. When what
is wanted is already obvious from the arguments, the guidance asks for the *why*
instead, since the reason is the part nobody can reconstruct from the record; for a
call that produces rather than reads — a write, an edit — the why is the whole field,
and the guidance says that is expected rather than a shortfall. So the field is never
legitimately empty, which is consistent with it staying required. The preamble also
tells the agent that triage may answer with a request for a better question
(`intent_feedback`, [redaction-fork.md](redaction-fork.md)) and that this is about
the intent, not the task. Collision is stated in one sentence: a tool that declares
its own `intent` keeps it.

**Readers.** The TUI leads a call entry's body with it. The fork peek's `result_view`
puts it in the result header so the triage `note` answers it rather than restating
what the tool did ([redaction-fork.md](redaction-fork.md)); the two halves are written
at different times by different requests, so they can disagree, and a `note` that does
not answer its `intent` is the signal that the call went sideways. When triage runs on
a separate model, the intent is the only place the agent's reasoning for a call
reaches it: the triage model reads the transcript but not the agent's thinking
blocks. Showing it to triage creates no persuasion incentive, because triage runs
after the call and cannot change whether it ran; what it can do is ask for a better
intent next time, through `intent_feedback`. The spotter is the case where the
persuasion concern does hold, and it does not see the field —
[permissions.md](permissions.md).

## Tool taxonomy

Not everything belongs behind the in-process trait.

**Built-in (in-process):** the generic, language-agnostic primitives — `fs`
(read / list / search / write), structured edit (`edit` /
`retarget_edit` / `undo_edit`), and `read_image`. These earn the typed hook:
gating, staleness checks, and line-numbered rendering for the fork all want
direct access to the result structure.

### `read_image`

`read_image` (`src/tools/read_image.rs` over the engine in `src/vision.rs`)
pulls an on-disk image into context as a `tool_result` image block
([log-and-projection.md](log-and-projection.md)) — the pull half of the
attachments design: an image enters the transcript only when the model asks,
at the resolution it asks for. The tool reads a `path` (resolved against the
session cwd, gated by the same read-path grants as `read`), decodes, optionally
crops to a requested `region`, sizes to the provider's own rule
(`vision::resized_size`), encodes, and returns a text header
(`{w}×{h} {media_type}, {n} visual tokens`, plus a parenthetical describing what
was read) followed by the image.

**Two limits bound the realized size**, and the header names which one bound,
because only one of them leaves a re-read worth trying. `long_edge` (default
1568, ceiling 2576) is the request's own cap; the visual-token budget (4784
patches on the high-resolution tier) is the provider's, and it binds first on
any frame past roughly 1930×1930 — a 2400×2400 source asked for at 2576
realizes 1932×1932 no matter what edge was requested. `read_image` derives which
by re-running `resized_size` with one limit relaxed at a time: if the size at
the maximum `long_edge` differs from the realized size, the request's
`long_edge` bound (advise a larger one); if it does not, dropping the token cap
tells the budget apart from the 2576-px edge ceiling. Naming the budget matters
because the alternative — advising a larger `long_edge` at the ceiling — costs a
read to learn that no such value exists.

**The region read is the escalation once a ceiling limit binds.** `region`
(`x`, `y`, `w`, `h`, in source pixels — the header reports the source frame's
dimensions to address into) crops **before** the sizing pass, so `long_edge` and
the budget bound the crop rather than the frame: a region small enough to fit is
encoded at 1:1 source resolution, sharper than any whole-frame downscale and
usually cheaper. The header echoes the realized rectangle and its origin
(`region 1200×900 at 100,200 of 4608×2592`) so coordinates inside the crop can
be offset back to the full frame.

Region validation splits on whether the request has a cogent reading. A shape
failure (non-object, a missing or non-integer field, a negative coordinate) and
a zero-area or wholly-outside-the-frame rectangle are `is_error` results — the
request was malformed. A rectangle that *overlaps* the frame but runs off its
far edge is **clamped**, and the header appends `clamped to the frame`: the
model is addressing from coordinates it remembers against a frame that may have
changed size, so the overlap is the answer to the question it asked, and the
echoed rectangle tells it exactly what it got. Erroring there would spend a
round trip to deliver information the header already carries.

Encoding policy: fitting API-native bytes pass through
verbatim; a crop, resize, or transcode re-encodes — JPEG stays JPEG (quality 85),
everything else becomes lossless WebP (the pure-Rust WebP encoder is
lossless-only). The tier limits are hardcoded to the high-resolution tier
until provider awareness supplies the session model's own; a standard-tier
provider downscales further server-side, costing bytes, never correctness.

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
fully reversible) that exposes its modules, `Tools` enum, and state struct, whose
`new(root)` constructor takes the session's cwd.

This section is about efference *consuming* MCP servers. It also serves one: the
session-control verbs are written as mcplease tools and mounted three times, as
`efference control <tool>`, as the stdio server `efference mcp`, and inside a hosted
session's own toolbox ([operating.md](operating.md)).

#### The `sessions` provider

A session created on a host mounts the control verbs as `Provider::Sessions`,
wrapping the control `Tools` enum in the in-process `McpServer<Control, Tools>`.
The session's model reaches its project's other sessions by tool call, under
namespaced wire names (`sessions__turn`, `sessions__state`, `sessions__create`).
`settle` is not mounted, and the `wait` flags on `turn` and `approve` are refused:
an in-session caller has its own inbox rather than a client that must block.

The provider's executor is `Executor::Host`, which holds the host and the calling
session's id and calls the same `Session` methods the HTTP handlers call — never a
round trip over the socket. The calling identity is what makes two rules
enforceable, and they are enforced by the executor rather than by config:

- **The caller is never its own target.** A call naming the caller, or naming no
  session (which defaults to it), is refused on every verb.
- **A mutating verb reaches descendants only.** `turn`, `approve`, `interrupt`,
  `archive`, `review_level`, `pace`, `title`, `background`, `dismiss`, and `proposal`
  require the target's `created_by` chain to reach the caller. `notify` and the
  reads (`sessions`, `state`, `transcript`, `entries`, `image`) reach any session
  in the caller's project.

While `state` shows the `cold` topic, a `turn` or an `approve` (other than `abort`) is
followed by a request that reads the whole context uncached (`context_tokens`). Both go
through; a driver that would rather not pay can fold first (`POST /loop/fold`). A turn
or decision is also the person action that releases a loop parked at its cold check
([control-api.md](control-api.md)).

`created_by` is per-session host control state: never logged, never projected, and
absent on a human root. It is also what the child's identity line names as its
creator, so a delegated session has its return address without being told one in
the task text ([context.md](context.md)). `create` from a session runs in the caller's cwd, takes the
caller's child environment as the new session's base, records the caller as the
child's `created_by`, subscribes the caller to the child's status (below), and defaults the child's review level to `upstream` — so a call the child's spotter holds comes back to the caller as a gate to answer with `approve` ([permissions.md](permissions.md)). Reach is the project: `sessions` lists, and every target
resolves, only among sessions whose `.efference/` storage base is the caller's, so
another project's id is not found rather than forbidden. `create` is bounded the same
way at the other end: a `cwd` whose `.efference/` base is not the caller's is refused
before anything is minted, since the caller could never reach the child it made. A `turn` submitted this way
carries `source: session[<caller id>]` ([log-and-projection.md](log-and-projection.md)).
The mount is unconditional for a hosted session today; which verbs a config mounts is
later work.

##### Subscriptions: a session's status in another's inbox

Waiting on another session is a subscription, not a blocking call. A session's
**status** — what it is doing at a decision point — reaches each subscriber's inbox
as a notification (docs/inbox.md) with the session's id as `source` and the slot
`session:<id>`, so a subscriber's pending inbox holds the latest condition rather
than a pile. The statuses posted are the decision points: **gated** (the held tool,
its intent, the spotter's reason, the `call_id`, and that `sessions__approve`
resolves it), **idle** after a turn completed (with the turn's closing prose,
truncated), **stalled** (docs/transport.md), **parked** (the loop stopped at its cold
check mid-turn: the cache is believed expired, so continuing pays the context uncached,
and a fold summarizes instead; docs/control-api.md), and **ended** or **failed** once the
runtime has returned. `running` is observed to detect the transitions and never
posted; the boot idle is not posted either. Every body says where the target's log
stands, so the subscriber pulls the transcript from where it last read. The status
is the current condition, not the child's prose: prose on every child turn would
grow the parent's cached prefix.

Two kinds of subscription, differing only in whether the notification must wake the
subscriber (docs/inbox.md — must-wake):

- **A creator's, made by `create`, with wake.** Delegation must wake: the parent gets
  a turn to act at each of the child's decision points even if it then decides
  nothing needs doing. In particular a child's turn ending — a report, a question,
  a refusal — hands control back to the parent without the child sending anything.
- **An explicit `subscribe`, without wake.** Observation may not: the subscriber's
  triage may dismiss or leave a peer's status. `subscribe` and `unsubscribe` reach
  any session in the caller's project, like `notify`; the creator's subscription
  is dropped by `unsubscribe` like any other. Neither is meaningful from an
  external client, which has no inbox: the HTTP executor refuses them and names
  `settle` and the `wait` flags instead.

The subscriber set is host control state on the target — never logged, never
projected — and the host runs one watcher per session that reads its status on
every control-state change and posts it when it moves (`src/daemon/subscriptions.rs`,
`src/daemon/status.rs`). A subscribed session counts as something that may wake
the subscriber, so its `wait` tool is not refused while one exists
(docs/supervisor.md).

##### Lifecycle: children end with their creator

A session ends when the session that created it ends, unless `create` passed
`detached: true`; the end is posted to the ending session's subscribers first, then
its own subscriptions are dropped and its non-detached children are ended, each
running the same teardown. Cascade is the default because the keepalive re-warms an
orphan's prefix every interval forever; `detached` exists for a session meant to
outlive its creator. The human sees every session in `sessions` either way.

##### Hand-off: a successor in the caller's place

`hand_off { brief }` gives the caller's work to a new session in the caller's own directory,
which resolves the project's config afresh — its grants, its preamble, its MCP servers — and
takes the caller's place (`Host::hand_off`, `src/daemon/host.rs`). It is `create` with the
placement changed rather than a `create` flag, because nearly everything that makes a child a
child is inverted:

- **Placement.** The successor's creator is the caller's creator, and it is detached exactly
  when the caller was: a successor of a human root is a human root, reviewing as
  `spotter_human` with its gates reaching the operator, where a child would default to
  `upstream` and answer to its creator. The caller is not subscribed to it.
- **Inference.** The successor is created with the options the caller was created with —
  model, spotter, triage, effort, temperature — and resolves them as the caller did, so a
  model named at creation carries across and one that came from the host's defaults or
  the project's `[transport]` comes from there again. A supervisor on a large model that
  hands off must not leave everything it supervises answering to the host's default.
- **The brief** is the successor's first turn, under the reserved source
  `handoff[<predecessor>]`: the clients draw it as the predecessor's brief rather than the
  operator's turn, and `POST /turns` refuses the prefix from a client.
- **Clients follow.** The predecessor's `session_identity` gains `successor`, and a client
  attached to it moves to the successor.
- **Archiving the predecessor** is the successor's, and the one exception to "a mutating verb
  reaches descendants only": `archive` also reaches the caller's own predecessor. The
  successor's identity line says so ([context.md](context.md)); it asks the predecessor what
  the brief leaves open with `notify` first, if anything.
- **Always the operator's call.** `sessions__hand_off` bypasses the fast path and the spotter
  to the human gate, as a write to the session's own config does (docs/permissions.md).

A client has no session to hand off, so the verb refuses from `efference control` and
`efference mcp` and names `create`.

`sessions__approve` is bound before it is classified (docs/permissions.md): the loop
asks the provider for the target's pending gate and gets the child's held action — tool,
input, intent, cwd, and the chain of sessions down to the leaf. The parent's fast-path,
spotter brief, and human gate read that action rather than the `call_id`, and the gate
view carries it as `acting_on` (docs/control-api.md). A held call that is itself a
`sessions__approve` is followed through the host to the leaf; a stale `call_id` refuses
with "gate moved" before any rung is consulted.

`McpServer<State, ToolEnum>` in `src/tools.rs` is generic over the
instantiations (fs-mcp, semantic-edit-mcp, and the session-control server). It holds
state in a `Mutex` — `mcplease`'s `Dispatch::call` is synchronous, so the lock never
spans an `.await`, and the loop it runs in is `Send`
([control-channel.md](control-channel.md)). Dispatching a call wraps
the name and arguments as `{"name", "arguments"}`, deserializes to the typed
`ToolEnum` via `serde_json::from_value`, and calls `Dispatch::call` against the
shared state with a `RequestContext` naming efference and declaring no client
capabilities (no wire, so the context that would ride in a request's `_meta` is
handed over directly). The resulting `CallToolResult` is folded by the same
renderer the connected providers use (`render_result` in `src/mcp.rs`): text
blocks land verbatim; image blocks are realized through the `src/vision.rs`
engine at `read_image`'s default long edge — sized, encoded once, and stored in
the result's `images` alongside a header line reporting realized size and
visual-token cost; audio and binary-resource blocks land as placeholders naming
what was withheld, as does an image that fails to decode. So an embedded tool
that grows a structured or multimodal result reads the same way as a remote
one. Any failure
(bad input or tool error) becomes an `is_error` result so the `tool_use` always
gets exactly one paired `tool_result` — the API rejects orphans.

#### A name no tool owns

`tools::unknown_tool_message` is what a call naming no tool gets back: the names
that do exist, and — since `bash` is always mounted — a pointer at it for a name
that is a program rather than a tool. `unknown tool: <name>` alone says what
failed, not what would work; in the incident behind this, the name was an allowed
bash prefix the session had met as a listed skill, and the peek then wrote a false
lesson the raw never claimed ("not available in this environment") onto the
canonical wire. A correction the result text states outright is one a summary can
compress but not invent past. The loop applies it ahead of the permission cascade
over the whole advertised set ([permissions.md](permissions.md)); the `Toolbox`
keeps its own copy for a name that reaches dispatch under no provider.

#### Which outcomes are errors

The first-party tools return a **refused or rejected** outcome as an error — a
`write` that would clobber an existing file, a flag conflict, an `edit` whose
placement did not validate, a failed `retarget_edit`. The refusal text is the
error's body, unchanged: fs-mcp's write-retry token still rides in it, and an
edit rejection still carries its diff and syntax errors. What changes is only the
flag, and the flag is what a reader sees first — a refusal shaped like a success
was read as a success, and the triage fork keeping the first line or two of the
body sharpened that (see `redaction-fork.md`).

An outcome is a success when it answers the call on its own terms even though it
changed nothing: a `search` with zero matches, a `find_anchor` that matched
nowhere, an `edit` whose exact change the file already contains, a
`retarget_edit` already aimed where it was asked to aim. The line is whether the
state the call asked for holds afterward — not whether the tool wrote anything.

The governing heuristic (jbr, 2026-08-27): unlike a POSIX return status, the flag
reports whether the tool was **wielded** successfully — was the request cogent.
Not finding results is not an error when the query was well-formed; the same
query malformed is. Overwriting without the documented overwrite flag is tool
misuse (or unexamined disk state — either way the wielding, not the world), so it
errors. A well-formed search that finds nothing is an empty success.

### The image gate

Whether a session's model accepts images is `ModelCapabilities::supports_image_input`
([transport.md](transport.md)), fetched at boot and handed to the toolbox
(`Toolbox::with_image_input`). It is `false` only where the metadata declares no
image input; unknown reads as capable, and an endpoint that then refuses an image
closes the gate mid-session (`Toolbox::refuse_images`, from the client's refusal
recovery in transport.md). It is applied at `Toolbox::run` — the one seam
every call passes through — rather than inside the image-producing tools,
because two unrelated backends produce images (`read_image`, and any MCP server
whose result carries an image block) and the gate must answer alike for both.

The two halves read differently on purpose:

- **`read_image` refuses before the read.** The tool exists only to produce an
  image, so it has no useful degraded form, and decoding a file to throw the
  pixels away spends the work to arrive at the same answer. The refusal is an
  error and names what would work instead — the sentence a model reads after a
  refusal is what decides whether it retries the same call.
- **A result that merely *carries* images keeps its text.** An MCP tool call is
  not *about* its image the way `read_image` is — a screenshot tool's text half
  is often the useful part — so the call still succeeds, `is_error` stays false,
  and only the bytes are dropped, with a line saying how many and why. What this
  buys is that the log and the wire stop carrying bytes nothing will look at, and
  the model is told rather than left to infer it from silence.

Without the gate, an image tool against a text-only model spends the upload on
either a 400 or on content the model cannot read, while its own success message
says it worked.

### Per-call telemetry

Every completed tool call appends a `Payload::ToolTelemetry` entry beside its
result (`src/agent/telemetry.rs`, derived at the dispatch seam in
`src/agent/step.rs`). It **projects to nothing** — log metadata in the same sense
timestamps are — which is what lets it be written unconditionally; the pin that
says so is `project::test::tool_telemetry_projects_to_identical_bytes`, the same
shape as the `Triage.thinking` pin.

It is a separate entry rather than a field on `ToolResult`, following the shape
`Spot` already uses for per-call audit: the pairing is by `call_id`, the payload
is written by a different concern than the result, and `ToolResult` is
constructed at dozens of sites — many of them a harness stand-in with no run to
measure — that would each have to name a telemetry value they do not have.

What it records:

- **Common, every call**: tool name, `call_id`, duration (the permission cascade
  excluded — a call that waited an hour at a human gate did not *run* for an
  hour), outcome class, result bytes **before** distillation, and realized image
  count.
- **Outcome class** is derived generically rather than per tool. Since built-in
  negative outcomes became `is_error: true` (above), `is_error` *is* the
  applied/rejected split; the third class, `harness_error`, is set only by the
  two sites that construct a harness-authored failure, never inferred from the
  text — inferring it would put the classification back where it started.
- **Bash**: the exit status the result reports (`"1"`, or `"signal"`; absent
  means the result reported none, which for a completed foreground command is
  zero) and whether the call asked to be backgrounded. The status is read back
  out of the result text by `bash::reported_exit_status`, which sits beside the
  writer that emits the marker — it never left that module in structured form,
  and widening `Outcome` to carry it would touch every backend for a field only
  telemetry reads. Also the `p#<n>` **handle** the supervisor minted for the
  command, when it reached a spawn ([supervisor.md](supervisor.md)) — the join
  from a call to the process namespace, and for a foreground command (whose
  registry entry is dropped when the call completes) the only typed place its
  ordinal survives.
- **Edit-shaped**: `operation`, anchor bytes, and content bytes, read off the
  call's own `input` under any of the spellings the mounted editing tools use.

The point is that grepping result *prose* answers none of them: "did that edit apply or get refused" was recovered from
`startswith("Applied")`, and anchor discipline was inferred from argument lengths
plus a count of `===TIP===` markers that turned out to be a throttle cap rather
than a measure. Prose written for a model is not a measurement surface.

**Not recorded in this cut**, because the data is not at the dispatch seam and
getting it means plumbing it out of the tool crates: the edit *rejection kind*
(anchor-not-found / syntax-guard / stale), and the shorter-anchor-sufficed
predicate behind the throttled tip. Both would be a change to what
`semantic-edit-mcp` returns, not to this layer.

### Schema translation

`McpServer::from_state` translates `mcplease`'s `ToolSchema` (MCP
`inputSchema`, camelCase) into the Anthropic `tools[]` shape (`input_schema`,
snake_case) via `anthropic_def`. `serde_json::to_value` normalizes object keys
to sorted order, so the resulting definitions are byte-deterministic for the
session and safe for the tools cache tier (see `transport.md` on caching).
Definitions are computed once at mount time and reused as the wire `tools[]`.

### Argument validation is the tool's, not the harness's

Nothing validates a call's arguments against the tool's schema before dispatch.
The in-process servers get strict checking for free — `run_call` deserializes the
`{name, arguments}` envelope into the generated tool enum, and a mismatch becomes
an `is_error` result naming it (`invalid input for read: invalid type: string
…, expected a sequence`). A connected server's arguments are passed through as
written and the server decides.

That asymmetry is deliberate rather than pending. Three checks already stand
between a model's call and a server: the provider constrains decoding to the
schema on the wire, the server validates its own input, and the in-process path
deserializes strictly. A fourth check in the harness would duplicate them and
carry a risk the others do not — a server that accepts undeclared extras, or
coerces a scalar, works today, and validating against a schema its author may not
treat as binding would turn those working calls into failures. The server's own
error is also written by the tool's author, so it is usually more specific than a
generic validator's.

The cost of passing a malformed call through is one wasted permission cascade:
the spotter rung runs before dispatch, so a call that was going to fail on its
arguments may pay for a verdict first. Observed rate across the recorded run
corpus is one malformed call in 319 tool results — and that one reached the
harness despite provider-side constrained decoding, so that mechanism is a
strong filter rather than a guarantee. `ToolTelemetry`'s outcome class makes the
rate readable per model, which is what a decision to add validation would rest
on.

Deferred tool discovery is the case where the reasoning inverts, because a
deferred call is not on the wire and so is never constrained-decoded; validating
against the fetched schema is part of that design (`backlog/deferred-tool-discovery.md`).

### Working directory: a root per mount

MCP `2026-07-28` has no sessions, so neither crate keeps a working directory a call
could change: a path is absolute or relative to the root the state was built with.
efference builds each server's state with the session's cwd, so every session gets
its own, and nothing is shared on disk — end-to-end tests are hermetic for the same
reason. semantic-edit-mcp's undo history and teaching progress live in that state
too, so they are per session as well; its `undo_edit` and `retarget_edit` name the
file the edit was made to.

Dispatch is async: the state is cloned into each call, and both crates run their
synchronous tool bodies on tokio's blocking pool, which is why they need a tokio
host.

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

### The `2026-07-28` standard request headers

A streamable-HTTP POST carries two headers derived from its own body:
`Mcp-Method` (the JSON-RPC `method`, on every request) and `Mcp-Name`
(`params.name` for `tools/call`/`prompts/get`, `params.uri` for
`resources/read`). Both are required for compliance; a value that cannot ride
raw in a header — non-ASCII, control characters, leading or trailing
whitespace, or one that would be mistaken for an encoding marker — is carried
as `=?base64?<standard base64 of the UTF-8 bytes>?=`.

The derivation and value encoding are `mcplease::headers::standard_headers` —
the same code a server validates with, so client and server cannot drift.
`src/mcp/http.rs` attaches them at the one point that already holds the
message, because what makes them useful is that they *agree* with the body: a
server that enforces them answers a disagreement with a `HeaderMismatch` naming
which field mismatched.

Enforcement varies, and that asymmetry is worth knowing: ferritin accepted
requests without them for as long as efference sent none, because ferritin is
built on the same mcplease whose server half does not validate them. Sentry
rejects them. A server built from the same library as the client will not catch
this class of bug.

### A declared server that does not mount stops the boot

`optional = true` on a stanza opts out; the default is that a mount failure
fails the session boot, naming the server and the reason. Declaring a server is
the operator's statement that this session should have it, so starting without
it produces a session missing a capability nobody knows is missing — and an
agent that answers "I can't do that" to a request the config says it can.
Boot is also the moment a human is present to fix it.

The flag is one boolean rather than a taxonomy of failure kinds: an uninstalled
command, an unreachable host, a TLS failure, a server awaiting authorization,
and a server writing non-JSON to its stdout all collapse to "did not mount".
Which one it was belongs in the message, not in the config surface. The default
is the strict setting deliberately — an opt-*in* to strictness is one nobody
remembers to write, so the safe behavior is what you get for free.

An optional server that fails is reported in two directions, because both
parties need it and neither can infer it:

- **The operator**, on stderr at every launch — not only the launch where it
  first broke. A config entry that quietly does nothing is the papercut this
  exists to prevent.
- **The model**, as the last section of the frozen system prompt
  (`mcp::unmounted_section`, spliced by `env::system_prompt`). Without it the
  model reads its tool table as the whole truth. With it, "that server is
  configured but did not connect: `<reason>`" replaces "I can't do that".

This is why mounting happens *before* the system prompt is built in
`src/bin/efference/serve.rs`: what failed to mount is part of what the session
must know, and the prompt is frozen once at boot (docs/log-and-projection.md).

### The client protocol, and what a transport is left holding

Wire protocol and serialization types both live in `mcplease`, taken with the
`client` feature. `mcplease::client::ClientProtocol` builds each request —
including the per-request `_meta` (protocol version, client identity,
capabilities) that revision `2026-07-28` requires, and that a server on any
revision reconstructs as its `RequestContext` — and classifies each arriving
message as a `Reaction`: a result to correlate by id, a reply to send (`ping`
answered, everything else refused as method-not-found, since efference declares
no client capabilities), a progress tick or a server message to route (below),
or nothing. `mcplease::client::Negotiation` runs the
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

The scope is every valid server-side interaction around a tool call (resources
aside). A `tools/call` opts in to progress (`_meta.progressToken`, the
request's own id) and each `notifications/progress` tick renders one line on
the call's live-output sink — pre-commit, never projected: progress is
liveness about a call running now, and the only reader it can serve is a
watcher. A `notifications/message` line becomes an inbox notification (source
the server, slot `mcp:<server>`, [inbox.md](inbox.md)) at every level; the
channel's noise level is unmeasured, and level-gating is the revision if a
chatty server proves noisy. An operator interrupt sends
`notifications/cancelled` for the abandoned call — on a single-channel
transport, where over streamable HTTP the dropped response stream is itself
the signal — and a response that still arrives is skipped by id. Everything
else stands as above: `ping` answered, undeclared capabilities refused
method-not-found, `tools/list_changed` noted and left alone (the table is a
mount snapshot). A server's chatter before it is mounted is dropped to the
debug log: negotiation and discovery run quiet, with no call to annotate and
no sinks that early.

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

The server's own tool names are not validated at config time — the MCP spec permits
characters (a dot, say) and lengths the request alphabet does not, and the namespace
prefix counts against the ceiling. So the frozen toolbox is checked against the
session's dialect at boot (`Toolbox::illegal_names`, `WireDialect::tool_name_problem`):
one alphabet, `[a-zA-Z0-9_-]`, and a ceiling of 128 characters on Messages or 64 on
Responses. An offending name fails the boot, reported in its display form with the
reason, rather than the first turn failing with a 400 that names nothing. Names are
not rewritten: a mapping would have to hold across the advertised definition, the
returned call, dispatch, the log, and replay, and the operator can rename the server
or hide the tool instead.

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

Every human-facing surface uses the dotted form: the transcript, the session drawer's
policy labels (including pending proposals, so a proposal reads and sorts as the
grant it becomes), and the config file the auto-persist write-back produces.
The wire and the log keep the underscores — the dot is applied at each display
boundary and undone on the way back in, so exactly one name is ever stored or
matched.

Server-wide clearance is the terminal-`.*` glob — `tools = ["ferritin.*"]` clears
every tool that server advertises, and `"efference.*"` the built-ins, whose
unprefixed wire names sit in the `efference` namespace
([permissions.md](permissions.md)). A bare `tools = ["ferritin"]` still clears
nothing: the namespace has to be named as one.

One known gap: the offline `wire` dev view does not mount connected providers
(mounting spawns processes), so a session with `[mcp.servers.*]` sends more
`tools[]` than that view shows.

### Duplicate tool name rejection: filter and guard

The API rejects duplicate tool names with a 400, so the mounted set must be
unique. fs-mcp's `move` and `delete` are listed in the `hidden` filter passed to
`McpServer::from_state`: they earn nothing over `bash`'s `mv`/`rm` and are not
worth the tool-doc tokens.

A `mounted_toolbox_advertises_unique_tool_names` test in `src/tools.rs` guards
against future collisions as new servers or tools are added.

## The session scratch dir and `$TMPDIR`

Boot mints a private per-session scratch directory (`<system-temp>/efference-<rand>/`,
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
end by the session runtime; `--keep-scratch` retains it as a post-mortem artifact.

## Open items

**cargo-mcp** is not embedded, and needs no embedding: as a stdio binary it
is one `[mcp.servers.cargo]` stanza away (the connected path negotiates down
to its protocol revision; no `mcplease` version alignment required). It
remains off the critical path per the taxonomy above — mount it when driving
cargo through bash shows real friction.

**Forwarding, renaming, and subsetting MCP tools** (the progressive-reveal
dimension of which of a server's tools reach the wire) is unsettled. Tools are
currently exposed as-is after the hidden filter; per-session or per-turn
subsetting is tracked in the backlog.
