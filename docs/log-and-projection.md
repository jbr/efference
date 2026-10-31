# The log, the projection, and caching

The log is the source of truth; the wire transcript is `project(log)`,
recomputed each turn. This doc covers the log's types, the projection, and the
caching constraint that shapes both.

---

## Core types

The types are `src/log.rs` and the files under `src/log/`, and the module imports no other
top-level module (docs/README.md, *Dependency direction*). That includes the shapes other
subsystems fill in or read: the provider and wire dialect a boot records (`provider.rs`),
a gate's `Approval` (`approval.rs`), the resolved `[context]` (`context.rs`), an attachment's
`Arrival` (`attachment.rs`), the spotter's `Axes` (`spot.rs`), the `TriageModel` a preamble
records (`preamble.rs`), and the names a recorded call is read by, `INTENT` and `ASK`
(`names.rs`). Behavior that needs another subsystem stays with it, as `impl` blocks on these
types.

### `LogEntry`

```rust
struct LogEntry { seq: u64, turn: u32, at: Timestamp, submission_id: Option<String>, source: Option<String>, payload: Payload }
```

`seq` is the monotone address space `project` reads over. `turn` is log
metadata — which agent turn produced the entry. `at` is a wall-clock
timestamp, also metadata only: timestamps are **never projected**, so their
non-determinism cannot touch the wire and cannot break caching.
`submission_id` is metadata of the same kind: the `POST /v1/turns` submission
a `User` entry realized (docs/control-api.md), present on every
socket-submitted turn and absent on entries the loop originates itself —
never projected, keyed on by clients reconciling delivery, and an audit
record of which submission produced the entry. It is skipped when `None`, so
pre-existing logs read back unchanged.

`source` differs from the metadata above: it records who submitted a
turn when that is not the operator — `web`, `mcp[<name>]`, or `session[<id>]` for
another session on the host — client-asserted like `notify`'s source. It **is**
projected: a `User` entry carrying one opens its wire block with a prefix naming the
speaker, so the model can tell the voices apart when several clients submit into a
session (docs/control-api.md). The stored `session[<id>]` opens the block as
`<id> (agent):` (`frontend::wire_label`), the spelling the preamble teaches
(`assets/prompts/session.md`), and `session[<id> in <place>]`, from a session in another
project, as `<id> (agent in <place>):`; every other label opens it as written (`web:`,
`mcp[<name>]:`). A pinned inbox note's source goes through the same label, so a note
from another project opens `⟢ <id> (agent in <place>):`. An `ask`'s reply from a source is prefixed the same way.
`None` on operator turns and on entries the loop originates itself, and skipped
when `None`, so an unsourced turn projects and serializes exactly as it did
before the field existed.

### `Payload`

```rust
enum Payload {
    User(Vec<ContentBlock>),
    Assistant { output: AssistantOutput, excluded: Option<Exclusion> },
    ToolResult { call_id: CallId, raw: RawResult, wire: WireState },
    ResultVerdict { for_result: Seq, wire: WireState },  // the final wire for a `Pending` result (see triage.md)
    Resumed(Resumed),  // the session was reopened by a later host process — projects a restart notice
    Archived { source: Option<String> },  // the session was archived on purpose, by `source` when named — never projected (see resumption.md)
    Summary(Summary),  // text replacing a range of the transcript — consolidation or compaction (see summary.md)
    Ambient(Notification),  // an out-of-band notification (see inbox.md)
    Triage(Triage),    // a triage verdict — the audit record (see triage.md)
    Spot(Spot),        // a spotter permission verdict — audit only (see permissions.md)
    Telemetry(Telemetry),  // how the session ran, not what it said — never projected (below)
    ProcessEvent(ProcessEvent),    // a background process's mint or exit (see process-monitor.md)
    ConfigEdit(ConfigEdit),  // one durable policy change — folded by the classifier (see permissions.md)
    Preamble { identity: String, body: String, context: Resolved, inference: Option<LoggedInference>, roles: Option<RecordedRoles> },  // boot record — replay/audit only (see context.md)
    Title(Title),      // the session's one-line title — presentation only, never projected (see session-title.md)
    Worktree(WorktreeEvent),  // the session's git worktree added, restored, removed, or kept — never projected (see worktrees.md)
    ImagesRefused { refusal: String },  // the endpoint refused images; the client withholds them from here — never projected (see transport.md)
}
```

```rust
enum Telemetry {
    Tool(ToolTelemetry),               // what one call cost and how it came out (see tools.md)
    CallStarted { call_id: CallId },   // a call's dispatch is about to reach the world
    RequestStage(RequestStage),        // one stage of one upstream request
    Push(PushRecord),                  // one step of a moment's push (see web-push.md)
    Cold(ColdEvent),                   // one step of the cold check (see control-api.md)
    CompactionRequested { warm: bool, cut: Seq }, // a compaction accepted, and its cut (see summary.md)
    CompactionFailed { error: String },           // the compaction asked for last ended without a summary
    InboxExpired { seq: Seq, shows: usize },      // a shown notification no verdict mentioned, dismissed by the harness (see inbox.md)
    SupervisorSet { supervisor: Option<String> }, // the supervisor pointer set or cleared (see permissions.md)
    Interrupt(InterruptOrigin),                   // an interrupt reached the loop: who sent it, through which control (see control-api.md)
    Continue(ContinueOrigin),                     // a continue was accepted: who sent it (see control-api.md)
    GateDecision(GateDecision),                   // a decision on a held call was accepted: the call, the decision, who decided (see control-api.md)
    Escalation(Escalation),                       // a held call passed up undecided: the call, who escalated, to whom, the note (see permissions.md)
    Consolidation { from: Seq, upto: Seq, context: Seq, tool_bytes: usize, outcome: String, account_bytes: usize },
    Tools { tools: Vec<Value> },                  // the tools[] the session advertises, as sent (below)
}
```

A telemetry entry serializes as `{"Telemetry": {"<Variant>": …}}`, and its log frame's
`kind` is `telemetry.<variant>` (`telemetry.tool`, `telemetry.cold`, …), so no frame kind
shares a name with a state topic. The family is the payloads every reader that renders a
session skips: the wire projection, the agent and TUI transcripts, and the web tree each
have one `Telemetry(_)` arm, so a new event of this kind is one variant. A reader that uses
a variant's contents matches it by name — the `[env]` masker (a failed request's reason),
the `current_operations` fold (which also folds the compaction's entries), the web client
attaching `Tool` to its call. Payloads that
project to nothing but are rendered somewhere (`ProcessEvent`, `Worktree`, `Title`,
`ImagesRefused`) stay top-level.

A `User` entry's blocks are prose and, after it, the files its composer
attached: `ContentBlock::Attachment` records a file's store name, path, and
triage signals, never its bytes, and projects as a text block holding the same
notice an ambient arrival carries ([inbox.md](inbox.md)). The notice is derived
from the recorded fields at projection time, so a change to its wording moves
the prefix only for a session that restarts on the new binary.

`Tools` records the `tools[]` the session advertises (`agent::tool_definitions`): appended
right after the boot `Preamble`, and at a resume when the array differs from the last one
recorded (resumption.md). The array is `Value`s, whose object keys serialize sorted, so equal
tool sets compare equal. It never reaches the wire; `dev wire` and the triage, primer and
compaction replays send the latest recorded array in place of one rebuilt from current code,
which carries none of the session's connected servers. A log that never recorded one falls
back to the rebuild.

`CallStarted` is appended after the permission cascade clears a call and before the
tool runs; an `ask` never gets one, since it waits on the operator and touches
nothing. A call with the marker and no result may have had side effects, which is
the one state a resumed session can't continue from, so resumption rolls back past that
completion. A call without it never ran, and resumption brings it back (resumption.md).

`Resumed` is appended when a later host process reopens the log. It records the gap
since the last entry, the background handles (`p#<n>`) with no recorded exit, and,
when the log ended uncleanly, `resumes_after`, the seq of the last entry the session
continues from, or, when the session resumed at an unfinished batch, `pending_calls`, the
calls still to come back, and, when the tools the session now advertises differ from the
last `Tools` record, `tools_changed`: the tool names added, removed or changed (or a bare
reorder), which the notice states. It projects as one text block at its own position
(`render_resumed`), appended to the wire like any later turn, so the prefix before it is
unchanged; with `pending_calls` the block is held until the results of those calls project
and lands in the same user turn after them (resumption.md). Entries
with a seq after `resumes_after` and before the `Resumed` entry are skipped by
`project_semantic` (`skipped_seqs`); a log with no such range projects exactly as
before. Each `Resumed` entry skips its own range.

`Archived` is appended when a session is archived on purpose (`DELETE /sessions/:id`,
`PATCH {"state": "archived"}`, a non-detached child ending with its creator, or an
island's exit), under the turn of the log's last entry. It projects to nothing. `source`
records who archived it, spelled as a turn's `source` is: `web` from the web client,
`mcp[<client>]` from a call over MCP, `session[<id>]` from a session archiving one it
drives, and `standalone exit` from an island's exit, which the host records itself. A
source a client sends is asserted and unverified. It is skipped when `None` (the operator's own
command line or TUI, a child ending with its creator, a session cancelled while
starting), and absent from every log written before the field
existed, which read back with `None`; adding it needed no version bump. The dormant inventory reads it as the row's `archived_by`,
and a client whose stream ended reads it to say who archived the session
([tui.md](tui.md)). The later of `Resumed` and
`Archived` is the session's lifecycle, which tells a deliberate archive from a session
that was open when its host stopped; a host shutdown appends neither
([resumption.md](resumption.md)).

`Telemetry` and `ProcessEvent` are log metadata in the same sense timestamps
are: both project to nothing, which is what lets them be appended mid-session —
on every tool call, and at every background spawn and exit — without moving the
cached prefix. `ProcessEvent` is the durable answer to "what was `p#3`" for a
reader with no daemon ([process-monitor.md](process-monitor.md)); metadata only, since a
process's output belongs to the registry's bounded best-effort capture.

These payloads are events the session went through that the model
never reads. They are timestamped by the entry's `at`, replayed by `GET /log` and
`/events` like any entry, skipped by the transcripts, and excluded from `project`, so
appending one mid-session leaves the cached prefix byte-identical (each has a test
pinning that). An event is recorded this way when a reader after the fact needs it
to explain what the session did; what streams per token stays on the ephemeral
`output` class instead.

`RequestStage` records where one upstream request is: `{request, origin, stage}`.
`request` is an id minted by the host when the request starts, unique within the
session and continued past the log's highest after a restart (the provider's
generation id arrives late or never). `origin` is the same `RequestOrigin` the
`usage.jsonl` lines carry (`turn`, `triage`, `title`, `spot`, `keepalive`,
`triage_primer`, `triage_keepalive`, `compaction`, `consolidation`, `aborted_attempt`). `stage` is `sent` (handed to the
transport), `attempt_failed {attempt, status}` (a refused attempt the transport may
re-send), `first_byte` (the first stream event), `ended` (the stream finalized), or
`failed {reason}` (an open failure, a stream that died or passed its ceiling, or
`interrupted`: an interrupt (the reason reads `generation interrupted`, and says nothing
about who sent it), or a request future dropped before it
finished; the reason is capped at 160 characters and passes the `[env]` masker). The
entry's own `at` is the stage's time. Streaming sub-stages (thinking, text, a tool
call) are not recorded; the ephemeral `output` class distinguishes them. The
decision-rung spotter's decisions request (docs/permissions.md) is one POST and one JSON
body: it records `sent`, `first_byte` when the response arrives, then `ended` or `failed`
under `spot`, one request id per POST (the cold question set and the relation set are two).

`Interrupt` records where an interrupt came from: `{source?, control?}`, both
client-asserted labels as `POST /loop/interrupt` received them (`source` spelled as a
turn's is, `control` naming the button or key). It is appended before the signal fires, so it
precedes the `failed: interrupted` stage of the request the signal ends, and it is
appended when nothing was running too. The `[env]` masker passes both labels.

`GateDecision` records a decision on a held call as `PUT /approvals/:call_id` or the
in-session approve accepted it: `{call_id, decision, source?, note_submission?}`, where `decision` is the
`Approval` the loop receives (`"Approve"`, `"Abort"`, `{"Redirect": "<message>"}`,
`{"ConfigEdit": {change, note}}`) and `source` is the client-asserted decider, spelled as
a turn's is and absent for the operator; `note_submission` is the submission id of the
user turn carrying the note an allow was sent with, which the transcript reads to show the
note on the call's line. It is appended once the call is known to be the
pending gate and before the decision reaches the loop, so it precedes the result or the
redirect's user turn it causes. A redirect the loop's own permission review writes (the
autonomous redirect, a `[commands]` deny under `autonomous`, a config edit's outcome) is
recorded too, under `review[<level>]` (`Session::review_redirect`); a call the loop clears
without a gate has none. The loop reads a redirect's decider from this record
(`Session::gate_decider`): the redirect's stub result names it (`<id> (agent) replied
instead of approving`, `the permission review declined it`, or the operator's "the user"),
and the user turn carrying the message carries the decider as its `source`, except the
operator's and the review's, so it opens on the wire as that decider's turns do. The
conversation transcript reads who allowed or denied a call from it
([control-api.md](control-api.md)). It projects to nothing
(`gate_decisions_project_to_identical_bytes`), and the `[env]` masker passes the
decider, a redirect's message, and a config edit's change and note.

`Escalation` records a held call passed up undecided (`decide` with `escalate`,
[permissions.md](permissions.md)): `{call_id, source?, to?, note?}`, where `source` is the
escalator spelled as a turn's is, `to` the session the call went to (absent for the
operator), and `note` the escalator's note to whoever decides next. It is appended when the
escalation is accepted, so it precedes the `GateDecision` that resolves the call. The
conversation transcript prints it on the call's line. It projects to nothing
(`escalations_project_to_identical_bytes`), and the `[env]` masker passes the escalator
and the note.

`Push` records one step of a moment's push (decided, held, released, cancelled, not sent
because a session answers the moment, or the push services' per-device answers). It projects to nothing, like `RequestStage`
(`push_records_project_to_identical_bytes`); its fields are specified in
[web-push.md](web-push.md).

`Cold` records the cold check ([control-api.md](control-api.md)): `parked {reason,
since, context_tokens}` when the loop stops before a request on the main prefix,
`released {by}` when a person acts while the session is believed cold (`by`: `turn`,
`approval`, `ask`, `continue`), and `unparked {cause}` when the park ends (`person`, or
`warm` when a fold or another request ended the belief). It projects
to nothing (`cold_records_project_to_identical_bytes`).

The stages are written from one place, `Client::send_request` in `api/client/send.rs` (and the
Messages keepalive in `api/client/keepalive.rs`), into a `StageSink` the session installs on the agent
client, the own-triage client, and the spotter's client. The caller tags the request
with `Client::with_origin`; the keepalive and the triage primer tag themselves. A
transport retry is attributed through a per-client slot holding the request in
flight, because the retry handler builds each follow-up on a fresh connection, so
requests that overlap on one client family can have a refused attempt attributed to
the later one. It projects to nothing, like `ToolTelemetry`, and is pinned by
`request_stages_project_to_identical_bytes`. Size: three entries of about 115 bytes
for a request that completes (about 350 bytes), one more per refused attempt. The
`current_operations` topic folds these entries ([control-api.md](control-api.md));
a `Resumed` entry closes every request still open before it.

### `AssistantOutput`

One model turn is stored in its provider dialect's ordered output vocabulary, not in
grouped `thinking` / `text` / `calls` fields:

```rust
enum AssistantOutput {
    Messages(Vec<MessagesOutputBlock>),    // Anthropic Messages content blocks, provider order
    Responses(Vec<ResponsesOutputItem>),   // OpenAI Responses output items, provider order
    Completions(CompletionsMessage),       // one Chat Completions assistant message, as received
}
enum MessagesOutputBlock { Text(String), Thinking { text, signature }, RedactedThinking { data }, ToolUse(ToolCall) }
enum ResponsesOutputItem { Reasoning { id, status, summary, encrypted_content },
                           Message { id, status, phase, content },
                           FunctionCall { id, call_id, name, arguments: String, status },
                           Unsupported(Value) }
```

Messages and Responses return an ordered array, and a provider may interleave freely (text before
thinking, several messages around one call). Keeping that order makes projection a copy
of provider-authored structure rather than a reconstruction: `project` maps each
`MessagesOutputBlock` to its wire block in place. The dialect sits outside the item vector
so one turn cannot mix replay credentials from two providers; the Messages projector emits
nothing for a `Responses` turn and the Responses projector will do the converse.

Consumers that do not care about the wire read the semantic accessors — `text()`,
`reasoning_text()`, `tool_calls()` — which flatten either arm in provider order.
`reasoning_text()` returns only human-readable reasoning; a Messages `signature`,
`RedactedThinking.data`, and Responses `encrypted_content` are replay credentials and never
surface through it. For a Responses function call the exact `arguments` string is the
replay authority; `tool_calls()` parses it on demand into a normalized `ToolCall` keyed by
`call_id` (the execution/result join key), never by the output item's own `id`.

A Completions turn has no item vocabulary of its own to type, because vendors put
reasoning in different places (`reasoning_content`, `reasoning` / `reasoning_details`,
`thinking` content parts). `CompletionsMessage` (`log/completions.rs`) is the
assistant message as the stream assembled it, an object of every key under its arrival
name. Assembly drops `null` values, `role` (the encoder writes it), and each tool
call's `index`. The object replays as stored. The accessors read the keys the dialect
defines (`content`, `tool_calls`, `refusal`); `reasoning_text()` reads every other key
by a display heuristic (a flat string value, or else the strings under `text`,
`summary`, `content`, and `thinking`), which never reaches the wire. The secret mask
([env.md](env.md)) rewrites `content`, `refusal`, and call arguments, and leaves the
opaque keys alone, as it leaves Messages thinking: they may be signed.

Logs written before this shape (grouped fields) are not read back; they were rewritten
once by a throwaway script under `scripts/`, which emits the old group order
(thinking, text, calls) because that is the only order the old record attests to.

`Assistant.excluded` is the assistant-side analogue of a `WireState` verdict:
`Some(exclusion)` marks a **record-only** entry, and `project` skips the entry
entirely. `Exclusion` (`src/log/exclusion.rs`) says why. `Upstream(record)`: the
stream that produced it died before finalizing, or finalized empty with no usage,
or, with no content at all, the request itself failed. `Empty`: the model finalized
with neither prose nor a call, and the loop asked again
([transport.md](transport.md)). The mark lives in the entry because projection is a
pure function of the log and cannot consult ephemeral transport state. `None` is the
default and is skipped when serializing, so finalized entries are byte-identical.

The record is a `why` plus, on the one path where nothing else survives — a
stream that finalizes empty — the transport evidence captured with it
(`StreamDiagnostics`, [transport.md](transport.md)). With no diagnostics it
serializes as a bare string, the shape every earlier log wrote, and both
spellings deserialize. Growing it cannot move the wire: the entry projects to
nothing either way.

`Preamble` records the full rendered `system` string once at boot, exactly as
this session's requests carry it, as two fields that concatenate to it: `identity`, the
line naming the model (`You are gpt-6.1-sol.`; empty in a log whose prompt predates the
line), and `body`, everything after it. It exists because the `[context]` preamble
builder ([context.md](context.md)) folds file *contents* into the system
prompt, and those files drift on disk between boots — so the preamble is no
longer reconstructable from `config + current files`, and faithful replay
requires storing it. It also carries the session's fixed main inference
identity as the log-owned `LoggedInference` shape: connection, provider, and
provider-native model id, the same values reported by
`session_identity.inference.main`. The log type is deliberately separate from the
live configuration selection; boot copies these fields into it once. The identity
is metadata, not prompt text or a derived placement, and `project` emits nothing
for it. Offline readers derive the dialect and provider-conditional placements
from that identity at read time. `dev wire`, including `--messages`, uses this
record without consulting inference configuration. A log predating it uses the
recorded dialect and default placements, with a note. The `system` field itself
is still read back rather than re-resolved from files on disk.

A resume that changes the spotter, triage or main model appends another `Preamble`: the
frozen one repeated, with `roles` (`RecordedRoles`: the spotter's `LoggedInference` and
the header's `TriageModel` shape) set. A main switch also replaces `identity` with the
new model's line and `inference` with its identity, and repeats `body` byte for byte, so
the new `system` differs from the old in that line alone. The header holds the roles
a session was created with; the latest preamble that carries `roles` overrides it at the
next resume, and one without leaves the header's standing. The entry projects nothing, so
the wire is the same with or without it ([resumption.md](resumption.md)).

### `WireState` / `Keep`

```rust
enum WireState {
    Pending,                                          // captured, awaiting its verdict: projects nothing
    Raw,                                              // project the full `raw`
    Triaged(Triaged),                             // the verdict, re-rendered by project
    Elided,                                           // project a placeholder
    TriageFailed { why: String },                     // no longer written (a triage without a verdict holds); older logs project a hard error
}
struct Triaged { keep: Keep, note: Option<String>, intent_feedback: Option<String> }
enum Keep { All, None, Ranges(Vec<(usize, usize)>) }
```

`WireState` is the one curation primitive. It lives next to an immutable
`raw: RawResult` on every `ToolResult` entry. **Curation only ever flips
`wire`; `raw` is never overwritten.** That is what makes every verdict
reversible: the full result survives in the log regardless of what the wire
shows.

A result the loop captured before its triage carries `Pending`, and its decision
arrives later as a separate `ResultVerdict` entry naming the result's `seq`.
Projection resolves a result's wire through that entry (`log::result_verdicts`) and
skips a result still pending; the verdict entry projects nothing at its own position.
The mechanism is in [triage.md](triage.md).

`RawResult` carries `content: String`, `is_error: bool`, and `images:
Vec<ResultImage>` — each image the tool returned as `{media_type, data, width,
height}`, with `data` the standard base64 of the encoded bytes stored **in the
log entry itself**, exactly as it appears on the wire. Storing the realized
bytes inline makes projection a copy of stored data: no re-read of a file that
may have changed, no re-encode whose output could drift across codec versions —
encoding happens once, at tool-run time. `images` is defaulted and skipped when
empty, so text-only results and pre-existing logs serialize byte-identically.
(The log file and the wire both keep `data` inline; the control channel is the
one surface that does not — its entry views omit it and serve the bytes at
`GET /log/:seq/images/:idx` instead — see [control-api.md](control-api.md).)

A fourth field, `content_elided: Option<ContentElision>`, is **written by
nothing in the log**: it is the control channel's tag for a `content` its window
and stream views truncated, carried on the shared type so a Rust client parses a
view with the log's own types (the mirror of `ResultImage::data`, which those
same views omit). Defaulted and skipped, so every logged line and every existing
log is unchanged.
On the wire, image placement is provider- and model-conditional
(`ImagePlacement`, chosen from the provider and the model slug once at boot and
fixed for the session — `project_with` takes it as its second input, so
projection stays deterministic: same log + same placement → same bytes). On Anthropic (`InToolResult`), a `tool_result`
whose entry has images takes the block-array content form (`[{text},
{image}…]`, the `[seq]`-prefixed text first). On OpenRouter
(`FollowingUserBlock`), the `tool_result` keeps the bare-string form and each
image trails it as a top-level `image` block in the same user message — the
portable shape, because OpenRouter's bridge drops `tool_result` images for
some upstreams (its Anthropic skin documents images as user content only; a
Gemini upstream 400s). On a Gemini slug through OpenRouter
(`AfterInterposedTurn`), the `tool_result` keeps the bare-string form and the
images leave the turn entirely: an assistant turn naming the result's `[seq]`
handle is projected next, then the images ride a user message of their own.
Gemini rejects a replayed thought signature whenever an image sits between the
signed `tool_use` and the next assistant turn — 400 "Corrupted thought
signature", sometimes surfaced as "Requests ending with a model turn are not
supported" — and no placement *within* the function-response turn avoids it
(image-in-`tool_result`, image-as-following-block, and a bare second user
message fail alike, on the provider's OpenAI-shaped endpoint as well as its
Anthropic skin). The interposed turn is the only shape that clears it, which
makes this the one arm that writes wire content no log entry produced; the
`[seq]` handle in its text is what binds image to result now that adjacency
does not. In every placement a text-only result projects the bare-string form
it always had, so adding images moved no existing bytes, and the placements are
byte-identical on any image-free log. Images ride `Raw`
and `Triaged` states — a triage verdict addresses text lines, and must not
silently eat an image the model pulled deliberately — and are shed by `Elided`
and `TriageFailed`, which is what makes elision the cost-recovery lever for
accumulated visual tokens. The triage projects the canonical prefix with
the same session placement (`Triager.image_placement`), or its request would
cold the shared cache lineage.

**The inversion `WireState` is not: masking.** `WireState` is *reversible
curation over immutable raw* — the verdict flips, `raw` survives. The `[env]`
table's masking ([env.md](env.md)) is the opposite arrangement: **irreversible
filtering upstream of the log**, applied at `Log::append_submission` before an
entry exists. There is no verdict to flip, because the value never becomes
`raw` — a raw copy on disk is half of what masking exists to prevent, so the
reversibility that makes `WireState` right for triage is exactly wrong here.
Folding masking into a `WireState` variant would reintroduce that artifact.
Byte-determinism of `project` is untouched either way: the filtering happens
upstream, so by the time projection reads an entry its text is already final.

Crucially, `Triaged` stores the **verdict** — which line ranges survive
(`keep`) and an optional breadcrumb (`note`) — not the rendered bytes.
`project` re-renders the kept hunks from `raw` each turn, so the wire is a
genuine pure projection, and the kept bytes are never persisted twice. The
structured decision doubles as the audit record. See [the triage doc](triage.md) for how verdicts are produced.

**Note on `Elided`:** `WireState::Elided` projects the placeholder `[elided]` —
a block with no content, present only so the paired `tool_use` isn't orphaned.
Its one construction site is the **interrupt** (`withheld()` in
[agent.rs](../src/agent.rs), see [control-channel.md](control-channel.md)): the
operator stopped a call whose output they did not want in the context, so `raw`
stays in the log and the wire carries nothing. Elision does *not* fold the paired
`tool_use` away — the pair is the reason the placeholder exists.

### `Summary`

```rust
struct Summary {
    from: Seq, upto: Seq,   // the inclusive range it replaces; a head summary has from == 0
    text: String,
    made_by: MadeBy,        // Consolidation (default, omitted when serialized) | Compaction(LoggedInference)
    audience: Audience,     // All (default, omitted) | Triage
}
```

A summary replaces the inclusive range `from..=upto` entirely inside `project`
and injects its text before the first retained entry after the range. A
consolidation's text is introduced by a harness-note line; a compaction's is
projected as written. Earlier summaries remain outside later consolidation
ranges. There is no mutation of stored entries; the replacement is a projection
concern, not a log mutation. See [summary.md](summary.md) and
[consolidate.md](consolidate.md).

### `project_semantic` and the encoders

```rust
fn project_semantic(log: &[LogEntry]) -> Vec<WireTurn>
fn encode_messages(&[WireTurn], ImagePlacement, BreakpointPlacement) -> Vec<ApiMessage>
fn encode_responses(&[WireTurn], preamble: &str, PromptCachePolicy) -> Vec<InputItem>
fn project(log) -> Vec<ApiMessage>   // = encode_messages(project_semantic(log), defaults)
```

Projection is two stages. `project_semantic` (`src/project/semantic.rs`) is the
dialect-neutral fold: it decides **selection** — which entries reach the wire, the
range replacement driven by a pre-pass over applicable `Summary` ranges, result verdicts
(`Triaged` re-rendered from `keep` + `raw`, `Elided`, `TriageFailed`), pinned
notifications, surfaced context, and every audit-only exclusion — and renders what
every dialect sends identically (result text with its `[seq]` handle, and the
`source:` prefix opening a sourced `User` turn's first prose part). Its output is
a list of `WireTurn`s:

```rust
enum WireTurn {
    User(Vec<UserPart>),          // prose, tool results, summaries, pins — log order
    Assistant(AssistantOutput),   // one model turn, owned (a fork tail appends synthetic ones)
    Breakpoint,                   // a cache-write boundary requested here
}
enum UserPart { Text(String), ToolResult { call_id, seq, content, is_error, images }, Breakpoint }
```

Consecutive `ToolResult` entries coalesce into one `User` turn, and a `User` entry
that directly follows them merges into that *same* turn as `[result…, text]` rather
than forming a second consecutive user turn, so role-alternation and byte-stability
hold when a user turn is appended right after a turn's results (a queued turn delivered mid-task,
[operator participation](control-channel.md#operator-participation--the-intervention-points) —
the mid-trajectory injection and the redirect steer both rely on this shape).

Each **encoder** (`src/project/messages.rs`, `src/api/responses.rs`) copies the turns
onto its wire and owns what differs there: the Messages encoder groups a user turn's
parts into one message, applies the three image placements and their interposed
turn, omits `is_error: false`, and attaches a `Breakpoint` per `BreakpointPlacement`;
the Responses encoder emits one `function_call_output` item per result and one user
message per run of prose, leads with the preamble as a developer message, replays a
logged Responses turn as the items the provider returned, and attaches a `Breakpoint`
as `prompt_cache_breakpoint` on the nearest preceding carrier ([transport.md](transport.md)).
The three byte pins fix the Messages encoder's output over the semantic fold; the
Responses envelope has its own pin (`responses_envelope_matches_expected_wire_bytes`).

The `Breakpoint` turn is how every request shape expresses its frontier without
touching wire types: the agent loop appends one at the tail, a triage places one
ahead of its raw fork-local tail, and the spotter marks its stable scope block mid-turn
with `UserPart::Breakpoint`. `Client::complete` takes turns, encodes for its dialect,
and sends — callers never build wire messages.

---

## Persistence — the log on disk

The log is persisted as **versioned JSONL** (`src/persist.rs`): line 1 is a
header, and every line after it is one entry. The mechanism — why JSONL at
all, and why an entry's line is never rewritten — is in `src/persist.rs`;
this section is the file format and what a reader does with a file that is not
one.

### The header

```rust
{"format":"efference-session-log","version":4,"metadata":{…}}
```

The header is one JSON object and nothing else. It is **not** a `LogEntry`: it
takes no `seq`, `project` never sees it, and the entries behind it are
numbered from 0 exactly as they would be without it. Being on disk rather than
in the projection, it cannot move the cached prefix however it is written.

`format` and `version` are the only values the header type can carry —
`format` names the file type and `version` the shape of the entries, a
monotonically increasing integer. Readers **require** the header: a file whose
first line is an entry is an unversioned log, not a v1 log missing a field, and
it is refused by name rather than read on the chance that an entry happens to
parse. A version this build does not know is refused before its entries are
read, and its metadata is held as a raw value until the version is accepted —
so a newer file's incompatible metadata reports as the unsupported version it
is rather than as a field this build happens not to know. There is no
importer for an unheadered log and no serializer kept for an old shape.

The reader accepts exactly one version, the current one: 7. Version 2 nested the telemetry
payloads under `Telemetry`; version 3 renamed `WireState::Distilled` to `Triaged` and the
`peek` request origin to `triage`; version 4 replaced the `Mark` entry with `Summary`
(`kind` and `note` became `from`, `upto` and `text`; `writer` became `made_by`) and renamed
the `narrate` request origin to `compaction`; version 5 split `Preamble.system` into
`identity` and `body`; version 6 renamed `Assistant.upstream_failure` to `excluded`
and gave it a kind (`{"Upstream": <record>}`, or `"Empty"`); version 7 renamed the
`Push` event `escalated` to `released` (docs/web-push.md). An incompatible change to the entries, including a change of
meaning that leaves their shape intact, increments the version. Before launch there is no
in-harness migration: the existing logs are converted once, outside the harness, with the
host stopped — the 1→2 pass is `scripts/migrate-log-v2.py`, the 2→3 pass (which also
rewrites `usage.jsonl`) is `scripts/migrate-log-v3.py`, and the 3→4 pass (likewise) is
`scripts/migrate-log-v4.py`, the 4→5 pass is `scripts/migrate-log-v5.py`, and the
5→6 pass is `scripts/migrate-log-v6.py`, and the 6→7 pass is
`scripts/migrate-log-v7.py`; the
unheadered→1 pass was the
2026-10-02 corpus migration (journal/2026-10-02-resumption-foundation-and-corpus-migration.md)
— and a file a pass missed is refused as the unsupported version it is.

Adding a payload variant is not a version bump: the one-time `scripts/migrate-log-archived.py`
appended `Archived` to every log then at version 4, so the first host that resumes at boot
reopens none of them.

There is no hash chain over the entries. A chain stored in the file doesn't stop an
editor from recomputing it or truncating the file, and every migration would have to
recompute it; tamper evidence would need an independently trusted checkpoint, which is a
separate concern from parsing and recovery.

`metadata` is the session boot recorded, in log-owned types
(`SessionMetadata`, `TriageModel`): the id and working directory, one
`LoggedInference` per role (main, spotter, and the triage selection when triage
does not share the main model, with the reasoning settings that inference is
called with), and the knobs and lineage a reader needs to describe the run —
temperature, effort, upstream sessions, `created_by`/`succeeds`, `autonomous`,
`keep_scratch`, `detached`. These are deliberately **not** the live configuration or
registry types: those describe what a host will do next, while a header
records what a past run did, and either side grows fields. Fields with an
obvious "off" value are skipped when unset, so a file records what was
configured and nothing else. It may be absent, which is how a fixture
persists; boot writes it.

### Creating

`Log::create` creates the file **exclusively** and writes and flushes the
header before returning, so a file that exists is already a readable log and
an existing file is an error left byte-for-byte untouched. Opening an existing
log for append while starting from an empty `entries` would restart `seq` at 0
inside a file that already numbers its entries — every offline reader
(`view`, `wire`, `triage`, the TUI fold) would read that as one sequence with
duplicates in it — so the refusal is what keeps that unreachable. The session
directory is minted exclusively for the same reason.

Creation also takes the **writer lock** below, which the directory already
implies; it takes it so that every sink holds one.

### The writer lock

A log has one writer, including across host processes. `Log::create` and
`Log::open` each take an exclusive advisory lock (`File::try_lock`) on the file
and hold it until the log is closed — the lock rides on the `File` the sink
owns, so it is released when the sink goes, after the last line is flushed.
`Log::close` drops the sink explicitly, and an archive calls it once its last
append (the worktree retirement) has landed: a `Session` can outlive its
teardown in whatever still holds it (an open SSE stream, a handler), and the
lock must not, or a resume right after the archive is refused. A closed log
keeps its entries for those readers and refuses appends; the SSE streams end
once they have delivered every entry. Dropping a `Log` that was never closed
releases the lock the same way.

A held lock is refused rather than waited on: the second caller is told which
log is in use (`OpenError::Held`, and an `io::Error` of kind `WouldBlock` from
`create`), instead of queueing behind a writer nobody asked it to wait for.
Reading takes no lock, so a session running elsewhere — or one being
reopened — can still be read by `load_recording`, the offline verbs, or
another host process.

### Reading

`load_recording` reads a log without touching it and returns the header, the
entries, and anything left past them; `load` is the entries-only wrapper for
readers that want just the record. A load has no side effects: it opens for
reading and writes nothing, whether it succeeds or fails, because the same file
is read again by inventory, by the offline verbs, and by the TUI. What it
checks, in the order a reader needs it:

- line 1 is a header of the format and version this build reads;
- every line after it is one entry — a blank line or an unparseable one is a
  hard error, because the log is a machine-written journal and a hole in one
  is something a reader cannot rule out (an older reader stepped over blank
  lines; nothing does now);
- `seq` equals the entry's index, so a duplicated or skipped `seq` cannot read
  as one continuous sequence to a consumer keying on it;
- bytes after the last complete entry are reported, never repaired.

### Load errors

A failure is a `LoadError`, which carries the path and the class of what went
wrong, so a caller can act on the class rather than on the wording:

- `Io` — the file could not be opened or read (its `io::Error` keeps its kind,
  so a missing recording and a permission failure stay distinct);
- `Unversioned` — line 1 carries no `format`: a log written before the header
  existed, or a file that is not a session log;
- `WrongFormat { found }` — line 1 names another file type;
- `UnsupportedVersion { version }` — this format, a version this build does not
  read, settled before the metadata is read;
- `Corrupt { line, message }` — line `line` is not what this version puts there:
  a header that is not a JSON object, a blank line, a line that is not an entry;
- `Sequence { line, expected, found }` — the entry on line `line` carries
  `found` where its position says `expected`.

`Display` is the text these errors have always printed, `"{path}:{line}: …"`,
which is what a person reads; the variants are for a program. `LoadError`
converts into `io::Error` (`InvalidData`, or the original kind for `Io`), so a
caller whose signature is already `io::Result` keeps its `?` — that is how the
offline verbs report a log they cannot load today.

### An interrupted append

A writer interrupted mid-append leaves bytes past the last complete entry.
Reading stops at the last entry that stands and *reports* the rest
(`Trailing`), because the two cases need different things and only a read can
tell them apart:

- `MissingNewline` — the last entry is complete and its `seq` is in order, and
  only the newline that would have closed it is gone. Every entry is usable;
  a writer appending there has one line to terminate.
- `PartialEntry` — an entry that was never finished: the last line in the
  file, with its bytes and the offset they start at.

The distinction is a parse that *ran out of input*, and only for the final
unterminated line. A complete object that is not an entry — an unknown payload
shape, a missing field — is damage rather than a torn write and fails whether
or not a newline closes it. Lines are read as bytes and decoded one at a time,
so a tear inside a multi-byte codepoint is reported with the rest instead of
failing the entries in front of it.

Nothing repairs a reported tail: reading a log and continuing one are separate
operations, and only the second has to reason about who else may be writing.

### Inventory of dormant sessions

`src/inventory.rs` lists a working root's sessions from disk without a host: one
row per directory under `<root>/.efference/` (`GET /sessions?state=archived`,
[control-api.md](control-api.md)). Per log it reads the header line, the first
entry, and the last complete entry by seeking from the end, then reads backwards
in 64 KiB chunks for the last `Payload::Title` and the last lifecycle entry
(`Resumed` or `Archived`), which sets the row's `interrupted`; the log is not loaded
whole. `created_at` is the `at` of the
first entry and `last_active_at` the `at` of the last complete one — file mtime
is not consulted, since a rewrite moves it without any session being active. A
torn final line without a newline that does not parse is skipped back over (see
"An interrupted append"); a first or last line that is complete and does not
parse, a missing or empty log, and a header refused by the same validation the
loader applies (`persist::read_header`) are rows with a `problem` string.

`Inventory` caches each row under its log path with the length the scan saw. A
refresh stats every log and rescans those whose length differs, which suffices
because logs are append-only; rows for directories that are gone are dropped,
and a read the filesystem refused is not cached.

### Continuing a log

`Log::open` is the writable half, and it is the whole of it, in an order where
each step is still the thing that can fail:

1. open the file for reading and writing, **without** creating it — a path with
   no log at it is `OpenError::Io`, since `create` is what mints a session;
2. take the writer lock (`OpenError::Held` if another writer has it);
3. read the log through the same strict reader every other reader uses, through
   the handle that holds the lock, and refuse anything it refuses
   (`OpenError::Load`) with nothing written;
4. resolve the reported tail, if there was one;
5. position at the end, so the next append takes the `seq` after the last entry
   read.

It returns the header as read, the log whose `entries` are those entries and
whose sink appends after them, and what the tail needed:

- `Repair::MissingNewline` — the one byte was written. Every entry read is
  kept, and the file is byte-for-byte what a writer that had got that far would
  have left;
- `Repair::PartialEntry { byte_offset, saved_to, bytes }` — the fragment is
  written to a sibling file first, named `log.jsonl.torn-<byte_offset>`, and
  the log is then truncated to `byte_offset`. The bytes are kept because they
  are the only copy of whatever the interrupted append was carrying; the
  truncation is what lets the entries behind them stand and the next append
  begin a fresh line. A fragment file already at that name is **not**
  overwritten — it is another repair's, and this one is about to leave the only
  file that holds its bytes — unless it holds exactly these bytes, which is a
  repair interrupted between the two steps and is finished rather than refused.

Both repairs `fsync` before returning, and the fragment is written *before* the
truncation, so an open that dies mid-repair leaves a log that still has the
bytes in it. The session's `[env]` mask is installed afterwards, by the caller
and through the same `set_masker` boot uses; an entry appended before then is
written with whatever mask was in force, which is none.

Nothing here changes what a read means: the fragment was never an entry any
reader accepted, so dropping it costs the append that did not finish rather than
an entry address. And a kept fragment is not a log: session discovery joins the
exact name `log.jsonl`, so a `log.jsonl.torn-<byte_offset>` sibling is not
picked up as one, and the reader would refuse it as unversioned if it were.

---

## Why byte-determinism is a caching correctness property

Prompt caching is a **prefix match**: any byte change anywhere in the prefix
invalidates every cached block after it. Render order is `tools → system →
messages`.

If `project` is not byte-stable — identical log, identical output, every
time — the cache silently breaks. Two guards in `src/project.rs` enforce this
and must not be weakened:

- `projection_is_byte_stable` — re-projecting an identical log yields
  identical bytes.
- `projection_matches_expected_wire_bytes` — pins the exact wire encoding. Any
  reorder, rename, or addition of a wire field fails this test on purpose:
  that change would silently bust the production cache, so it must break a
  test first.

Both pins, and the whole-envelope pin in `src/api/test/envelope.rs`, answer one question:
does the harness still emit the bytes it emitted yesterday? They cannot answer
whether the **provider** still matches those bytes — a dependency's serialization
changing under us, a provider-side normalization, a field that varies per session.
That half is empirical, and its instrument is the read rate below.

The primary determinism hazard: `HashMap` JSON key order is per-process
random. Tool-call `input` is a `serde_json::Value` specifically because it
serializes object maps with sorted keys. A `HashMap` here would kill caching
silently. When adding wire-facing fields, keep them deterministic and update
the expected-bytes pin deliberately.

---

## Verified API facts (Anthropic)

| Fact | Value | Note |
| --- | --- | --- |
| `cache_control` breakpoints / request | **4 max** | place deliberately; walk them forward as the session grows |
| Prefix lookback | **20 content blocks** | a breakpoint only finds a cache entry within the last 20 blocks; long agentic turns blow past this — drop intermediate breakpoints every ~15 blocks |
| Min cacheable prefix (**Opus 4.8**) | **4096 tokens** | ⚠️ *not* the ~1024 the origin chat assumed (that's Sonnet-4.5-era). Sub-4096 prefixes **silently** don't cache on Opus 4.8 (Fable 5 / Sonnet 4.6 = 2048) |
| Cache write cost | **1.25× (5m TTL), 2× (1h TTL)** | base input price |
| Cache read cost | **0.1×** | the lever |
| Break-even | 2 requests (5m), 3 requests (1h) | |
| Invalidation tiers | tools / system / messages | changing `tool_choice`, toggling `thinking`, adding images invalidate *system+messages* but **not** the tools cache; changing a tool def or the model invalidates everything |
| Concurrent timing | a cache entry is readable only **after the first response begins streaming** | N parallel identical-prefix requests all pay full price — fire one, await first token, then fan out |
| Determinism hazard | `HashMap` JSON key order is per-process random | serialize tool-use inputs via `BTreeMap` or `serde_json` `preserve_order`; a non-stable projection silently kills caching |
| Mid-conversation system msgs | **Opus 4.8**: append `{role:"system"}` to `messages[]` | does **not** invalidate the cached prefix; a non-spoofable operator channel — the natural merge point for ambient/operator events |

> **TTL is a write-time property.** A warm 5m cache cannot be read-upgraded to
> 1h — `ttl` attaches only to `cache_creation` tokens, so a 1h lifetime must
> be chosen at a cold write. Keeping the cache warm across idle (a
> `max_tokens:0` keepalive read that refreshes the sliding TTL while generating
> nothing) and making cache usage legible are transport concerns realized in
> [`src/api.rs`](transport.md) — never log or projection changes, so they
> cannot perturb the byte-stability pins.

---

## Breakpoint placement — provider-conditional

`place_trailing_cache_breakpoint` (`src/project/wire.rs`) marks exactly one block
per request, walking back from the wire tail. **Which** block it may mark is
`BreakpointPlacement`, derived from the session's provider (`Provider::breakpoint_placement`)
and fixed for the live session. A recorded log carries the provider identity,
not the derived placement, so an offline reader derives this choice at read
time; a legacy log without that identity uses the default placement and says
so:

- `TrailingBlock` (**Anthropic**) — the last block, whatever its type.
  `cache_control` is legal on any block on the native API, so the marker sits at
  the true tail and nothing is left behind it.
- `LastTextBlock` (**OpenRouter**) — the last **text** block; trailing
  `tool_use`/`tool_result` blocks stay unmarked, and a wire with no text block at
  all gets no marker. OpenAI's explicit prompt caching — which OpenRouter
  converts Anthropic-format markers into on OpenAI-fronted endpoints — attaches
  breakpoints only to text blocks: a marker converted onto any other block type
  bills the full 1.25× write on **every** request and never produces a readable
  entry (measured; this was most of an audited session's bill). The cost of the
  portable placement is that the unmarked suffix re-bills at the input rate each
  request until a text block (a user turn, a pinned notification, a
  surfaced-context reminder) reappears.
- `TrailingBlock` also for **`anthropic/` slugs on OpenRouter**: the marker
  passes through to Anthropic, whose cache reads only up to it. Probed
  2026-10-07 on `anthropic/claude-haiku-4.5`: under `LastTextBlock` the read held
  at 7,592 tokens across a six-round tool loop while uncached input grew ~1.2k a
  round; trailing, the read advanced every round and each request wrote only its
  own delta.
- `None` (**OpenRouter, Gemini slugs**) — no marker at all, chosen from the slug
  the way `ImagePlacement` picks its Gemini arm. OpenRouter turns a marker into a
  Gemini explicit cache of the prefix up to it; a request carrying a marker reads
  only that cache, and a marker on a `tool_result` (or on text nested inside
  one) creates none. Under `LastTextBlock` the read therefore froze at the last
  text block for a whole tool loop while the uncached tail grew by each round.
  Unmarked, Gemini's implicit cache follows the prefix in coarse steps (~4k
  tokens, with an occasional full miss); over a 15-round probe loop that cost
  less than half the frozen placement (2026-10-07). Each explicit-cache creation
  also reported `input 0 · cache_read N · cache_write N` and billed about 1.25×
  an uncached prompt; OpenRouter's generation record counts the written tokens
  in both `native_tokens_prompt` and `native_tokens_cached`, which reads as the
  prompt billed twice.
  Creations followed every marker move and every switch of the upstream
  provider (`Google` ↔ `Google AI Studio`), since explicit caches do not cross
  providers; with no marker there is no creation.

On the Responses dialect the marker is `prompt_cache_breakpoint: {"mode":"explicit"}`
on an `input_text` / `input_image` part, sent only when the model's cache policy is
`Explicit` (GPT-5.6 and later; the request also sets `prompt_cache_options.mode`).
Assistant items are not carriers, so the marker walks back to the last user part —
a bare-string `function_call_output` is lifted to its parts form to carry it — and
the physical marker can sit earlier than the logical frontier. Under any other
policy the encoder places nothing and the provider's implicit breakpoint governs.

The placements are the same interface `ImagePlacement` uses. A live session fixes
both choices at boot so its cached prefix cannot move under it; an offline
reader re-derives them from the recorded provider and model, so a log rendered
under a different current config still has the historical shape. The log does
not store either derived value. The consequence for the byte pins is that the
pinned bytes are pinned **per placement** — `project/test/cache.rs` covers the
arms and asserts the marker count stays one for each. On the Responses wire the triage's frontier has its own
pin (`triage/test.rs`): the triage's one marker sits on the last carrier before the
fork-local tail and never in it, and the next canonical request repeats the pre-fork
items content for content, marks its own tail, and carries the selection rather
than the raw output or the triage's ask. The marker is a write instruction, not
content, so the two requests differ only in where it sits.

One request shape rides this marker everywhere: agent turns, triages, and
keepalive pings send the same `tools[]`, `tool_choice: auto`, and thinking policy
([triage.md](triage.md), the shared envelope), so a session's
requests read and extend a **single cache lineage** rather than keying one per
request shape.

---

## The per-request usage record

Every upstream request appends one JSONL line to the session dir's `usage.jsonl`:
a wall-clock stamp, the request's origin (`turn` / `triage` / `spot` /
`keepalive` / `aborted_attempt`), the upstream response id (Anthropic `msg_…`; an
OpenRouter-fronted request's generation id), the model the response named for
itself, and the four token counts. When the response reported them, the line
also carries `served_by` (the upstream that served the request) and `cost_usd`
(the billed cost reported inline, docs/transport.md); both are omitted
otherwise. When the provider named a cache-miss reason, the line carries
`cache_miss` — the provider's own object, in its spelling
(`{"type":"tools_changed","cache_missed_input_tokens":41850}`) — and it is absent
otherwise, which is most lines. The main model and provider are fixed when
a session is created, but usage remains per request: an upstream may report a
model alias or a response may be retried, so the per-line value is the
accounting record. The canonical log carries the boot identity separately;
`dev wire` does not read this telemetry file.
Implemented in `src/daemon/metrics.rs` (`record_request` / `record_keepalive`),
fed by `Completion::request_usage`. Transport-only and **never** the canonical
log — usage must stay out of the projection. The in-memory `Metrics` fold
surfaced by `GET /state` is unchanged; the file is what makes spend
reconciliation a query (joinable on the response id against provider accounting)
rather than a reconstruction from the canonical log.

`RequestOrigin` is a persisted enum, so it carries an `Unknown` fallback
(`#[serde(other)]`) for any value a newer build might write that this one
predates — an old reader must still parse a new file's lines instead of
dropping them.

`aborted_attempt` tags a retried attempt — one line per attempt, recorded
before the loop retries (docs/transport.md). An unfinalized stream records
all-zero usage; a stream that finalized unparseable records the real counts it
billed. It is real telemetry (the record-only partial it accompanies in the log
needed exactly this kind of durable per-attempt trail) but it is never cache
evidence, and only its non-zero usage is priced: `spawn_cost_fetch` skips the
all-zero case (nothing to price, and fetching it would only run the lookup to
exhaustion and mark it `unpriced`), and `ColdStreak` excludes it from
the streak while still treating it as a touch on the prefix (below).

### The cache-read oracle

`cache_read` on those lines is the empirical end of the byte pins. A main-loop
request that reads nothing from the cache is **cold**, and three conditions
explain a cold read without implicating the prefix: the session's first request
(nothing was written yet), rendering or replaying under a different model (a
different model shares no prefix), and a gap past the five-minute TTL with
nothing touching the prefix (evicted, not moved). Anything else is one
observation of the prefix having moved.

One observation is noise — a retry, an eviction the TTL heuristic misses — so the
signal is the **run**. `ColdStreak` (`src/daemon/metrics.rs`) folds each recorded
request into a consecutive-unexplained-cold counter: only `turn` requests move it
(the side channels carry their own prefixes at their own lineages), `keepalive`,
`aborted_attempt`, and a `triage` that ran on the agent's client (its usage line
carries `main_prefix`) stamp the touch clock the idle test reads without counting,
and any warm read clears it. An aborted attempt's zero
`cache_read` is not evidence the prefix moved — counting it drove a false
streak (two aborted attempts on one turn moved the
streak to 2). `COLD_STREAK_WARN` is 4, set
against this repo's own corpus:
across 1371 recorded turns, unexplained cold reads formed 28 runs — 24 of length
1, two of length 3, and two of length 28 and 43, each a whole session that never
read the cache once. The constant's doc comment carries the full fold.

A read rate is a **request**-level reading, and calls a request warm on any nonzero
read: one that matched the preamble and then paid for the rest of a 50k context counts
the same as one that matched everything. The report states the uncached input tokens
beside the rate for that reason — 81% warm turns against 2.07M uncached input is the
shape a rate alone hides. It is a recorded fact and not a model: what a request was
*entitled* to reuse depends on where the breakpoint landed, the model's cache policy,
and the upstream's own behaviour, and none of those are in this record. Asking the
provider is the better instrument where one exists — the Codex response's
`usage.attribution` carries per-input-item cached counts, and OpenAI's platform API
answers `prompt_cache_options.comparison_response_id` with a `prompt_cache_diagnostics`
reason naming what diverged.

Anthropic answers a top-level `diagnostics` object on the Messages dialect with its own
naming of the first divergence, which is wired here: `src/api/client.rs` carries the
previous response id per client and `CacheMiss` rides the usage line as `cache_miss`
(docs/transport.md, under "The provider's own cache diagnostics").
`efference dev cache` counts those reasons across a record — the four `*_changed` ones
apart from `previous_message_not_found` and `unavailable`, which say that no comparison
happened rather than what moved. It is attribution beside the streak, not a replacement
for it: the reasons explain a cold read whose cause is in the request, and a streak of
cold reads with no reason attached is still the signal this oracle exists for.

A separate triage model has its own prefix, so its reads are counted apart:
`usage.triage_cold_streak` runs the same classifier (`ColdStreak::observe_triage`) over
result triages and primers on the triage client, with the triage keepalive ping as a
touch and not as evidence. It is null while the triage borrows the agent's client, whose
reads the main count already holds. A status reading with no threshold attached.

Two surfaces read it. `GET /state` publishes the live count as
`usage.cold_streak`, which the TUI header states beside the context meter once it
crosses the threshold and drops when a warm read arrives. `efference dev cache`
(docs/operating.md) replays a recorded `usage.jsonl` through the same `ColdStreak`
offline — per-origin read rates, the explained cold starts, and the longest run —
for one session, or `--all` for the corpus. It also reports the abort rate (aborted
attempts as a fraction of turn attempts): classifying `aborted_attempt` as its own
origin means those lines no longer read as unexplained cold turns in the report.

---

## The invariant tying caching to triage

**Committed regions are immutable on the wire; all mutation happens in the
uncommitted tail past the last cache breakpoint.** Three operations must honor
this:

- **Triage is at append.** Each tool result's `wire` is fixed the moment it
  enters the log and never changes — so no cached prefix entry is ever
  mutated. The invariant holds by construction.

- **A summary replaces a named range.** A `Summary` elides `from..=upto` and
  injects its text; the retained prefix before `from` stays byte-stable. A head
  summary (`from == 0`) replaces everything through `upto`. Both deliberately
  invalidate prefix-cache reuse from the replacement's start onward, while
  retaining the content after the range.

- **Ambient notifications** never splice behind a breakpoint. They arrive
  out-of-band and are logged at arrival (a slot notification, at first show). A pinned notification projects
  at the position of the `Triage` that pinned it — always tail-ward — so the
  wire grows only at the tail, never behind a cached block. Arrival entries
  (`Payload::Ambient`) project to nothing on the canonical wire. See
  [inbox.md](inbox.md).

Because triage-at-append never touches a committed entry, no explicit freeze
primitive is needed. Keeping the canonical lineage
lean — triaged, not raw — also keeps consecutive breakpoints inside the
20-block window longer.

---

## The wire is a pure projection; materializing it is pure memoization

`project` is a deterministic fold, so an in-memory materialized wire is a memo
with no semantic effect — drop it and rebuild byte-identically from the log.
Correctness can never depend on the memo.

Its invalidation boundary coincides with the prompt-cache breakpoint: every
tail append extends the wire only at the tail, and the one event that rewrites
the prefix — a summary — is exactly the "one deliberate cache
write." Incremental re-derivation follows the same prefix/tail boundary the
cache already imposes.

The memo is available as a pure-efficiency lever but is **never persisted** — a
persisted wire would become a rival source of truth. Tracked in the backlog
until the transcript is long enough to measure.

### Opportunistic consolidation usage

`consolidation` is a continuation of the main model's full
projected context, with its instruction after the shared-prefix breakpoint
(`docs/consolidate.md`). Its usage records count as a main-prefix read, but it
does not send the loop's exact prompt or become the diagnostics comparison base.
`Telemetry::Consolidation` records the frozen bounds, examined context sequence,
post-verdict tool-text budget, disposition and adopted account byte count. It
projects to nothing; only an adopted summary contributes account text.
