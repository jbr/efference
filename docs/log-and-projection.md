# The log, the projection, and caching

The log is the source of truth; the wire transcript is `project(log)`,
recomputed each turn. This doc covers the log's types, the projection, and the
caching constraint that shapes both.

---

## Core types

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
turn when that is not the operator — `web`, `mcp[<name>]`, or `agent[<id>]`,
client-asserted like `notify`'s source. It **is** projected: a `User` entry
carrying one opens its wire block with a `source:` line, so the model can tell
the voices apart when several clients submit into a session (docs/control-api.md).
`None` on operator turns and on entries the loop originates itself, and skipped
when `None`, so an unsourced turn projects and serializes exactly as it did
before the field existed.

### `Payload`

```rust
enum Payload {
    User(Vec<ContentBlock>),
    Assistant { output: AssistantOutput, upstream_failure: Option<UpstreamFailureRecord> },
    ToolResult { call_id: CallId, raw: RawResult, wire: WireState },
    Mark(Mark),        // a salience annotation
    Ambient(Notification),  // an out-of-band notification (see inbox.md)
    Triage(Triage),    // a fork verdict — the audit record (see redaction-fork.md)
    Spot(Spot),        // a spotter permission verdict — audit only (see permissions.md)
    ToolTelemetry(ToolTelemetry),  // what one call cost and how it came out (see tools.md)
    ProcessEvent(ProcessEvent),    // a background process's mint or exit (see supervisor.md)
    ConfigEdit(ConfigEdit),  // one durable policy change — folded by the classifier (see permissions.md)
    Preamble { system: String, context: Resolved, inference: Option<LoggedInference> },  // boot record — replay/audit only (see context.md)
    Title(Title),      // the session's one-line title — presentation only, never projected (see session-title.md)
    // Anticipated, not yet built: a frozen cache-breakpoint boundary (`Commit`)
    // for compaction. Kept here as a forward marker — types land ahead of the
    // feature that constructs them — not present in the code today.
}
```

`Commit` is a placeholder: when compaction needs an explicit freeze primitive
it slots in here without touching any other type.

A `User` entry's blocks are prose and, after it, the files its composer
attached: `ContentBlock::Attachment` records a file's store name, path, and
triage signals, never its bytes, and projects as a text block holding the same
notice an ambient arrival carries ([inbox.md](inbox.md)). The notice is derived
from the recorded fields at projection time, so a change to its wording moves
the prefix only for a session that restarts on the new binary.

`ToolTelemetry` and `ProcessEvent` are log metadata in the same sense timestamps
are: both project to nothing, which is what lets them be appended mid-session —
on every tool call, and at every background spawn and exit — without moving the
cached prefix. `ProcessEvent` is the durable answer to "what was `p#3`" for a
reader with no daemon ([supervisor.md](supervisor.md)); metadata only, since a
process's output belongs to the registry's bounded best-effort capture.

### `AssistantOutput`

One model turn is stored in its provider dialect's ordered output vocabulary, not in
grouped `thinking` / `text` / `calls` fields:

```rust
enum AssistantOutput {
    Messages(Vec<MessagesOutputBlock>),    // Anthropic Messages content blocks, provider order
    Responses(Vec<ResponsesOutputItem>),   // OpenAI Responses output items, provider order
}
enum MessagesOutputBlock { Text(String), Thinking { text, signature }, RedactedThinking { data }, ToolUse(ToolCall) }
enum ResponsesOutputItem { Reasoning { id, status, summary, encrypted_content },
                           Message { id, status, phase, content },
                           FunctionCall { id, call_id, name, arguments: String, status },
                           Unsupported(Value) }
```

Both wires return an ordered array, and a provider may interleave freely (text before
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

Logs written before this shape (grouped fields) are not read back; they were rewritten
once by a throwaway script under `scripts/`, which emits the old group order
(thinking, text, calls) because that is the only order the old record attests to.

`Assistant.upstream_failure` is the assistant-side analogue of a `WireState`
verdict: `Some(record)` marks a **record-only** entry — the stream that produced
it died before finalizing (or, with no content at all, the request itself failed;
[transport.md](transport.md)) — and `project` skips the entry entirely. The mark
lives in the entry because projection is a pure function of the log and cannot
consult ephemeral transport state. `None` is the default and is skipped when
serializing, so finalized entries and pre-existing logs are byte-identical.

The record is a `why` plus, on the one path where nothing else survives — a
stream that finalizes empty — the transport evidence captured with it
(`StreamDiagnostics`, [transport.md](transport.md)). With no diagnostics it
serializes as a bare string, the shape every earlier log wrote, and both
spellings deserialize. Growing it cannot move the wire: the entry projects to
nothing either way.

`Preamble` records the full rendered `system` string once at boot, exactly as
this session's requests carry it. It exists because the `[context]` preamble
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

### `WireState` / `Keep`

```rust
enum WireState {
    Raw,                                              // project the full `raw`
    Distilled(Distilled),                             // the verdict, re-rendered by project
    Elided,                                           // project a placeholder
    TriageFailed { why: String },                     // no longer written (a gate without a verdict holds); older logs project a hard error
}
struct Distilled { keep: Keep, note: Option<String>, intent_feedback: Option<String> }
enum Keep { All, None, Ranges(Vec<(usize, usize)>) }
```

`WireState` is the one redaction primitive. It lives next to an immutable
`raw: RawResult` on every `ToolResult` entry. **Redaction only ever flips
`wire`; `raw` is never overwritten.** That is what makes every redaction
reversible: the full result survives in the log regardless of what the wire
shows.

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
and `Distilled` states — a distill verdict addresses text lines, and must not
silently eat an image the model pulled deliberately — and are shed by `Elided`
and `TriageFailed`, which is what makes elision the cost-recovery lever for
accumulated visual tokens. The fork peek projects the canonical prefix with
the same session placement (`ForkPeek.image_placement`), or its request would
cold the shared cache lineage.

**The inversion `WireState` is not: masking.** `WireState` is *reversible
curation over immutable raw* — the verdict flips, `raw` survives. The `[env]`
table's masking ([env.md](env.md)) is the opposite arrangement: **irreversible
filtering upstream of the log**, applied at `Log::append_submission` before an
entry exists. There is no verdict to flip, because the value never becomes
`raw` — a raw copy on disk is half of what masking exists to prevent, so the
reversibility that makes `WireState` right for redaction is exactly wrong here.
Folding masking into a `WireState` variant would reintroduce that artifact.
Byte-determinism of `project` is untouched either way: the filtering happens
upstream, so by the time projection reads an entry its text is already final.

Crucially, `Distilled` stores the **verdict** — which line ranges survive
(`keep`) and an optional breadcrumb (`note`) — not the rendered bytes.
`project` re-renders the kept hunks from `raw` each turn, so the wire is a
genuine pure projection, and the kept bytes are never persisted twice. The
structured decision doubles as the audit record. See [the redaction
fork](redaction-fork.md) for how verdicts are produced.

**Note on `Elided`:** `WireState::Elided` projects the placeholder `[elided]` —
a block with no content, present only so the paired `tool_use` isn't orphaned.
Its one construction site is the **interrupt** (`withheld()` in
[agent.rs](../src/agent.rs), see [control-channel.md](control-channel.md)): the
operator stopped a call whose output they did not want in the context, so `raw`
stays in the log and the wire carries nothing. Elision does *not* fold the paired
`tool_use` away — the pair is the reason the placeholder exists.

### `Mark` / `MarkKind`

```rust
struct Mark { kind: MarkKind, note: String }
enum MarkKind {
    Boundary { upto: Seq },  // compaction fold-point — the only kind built
    // decision / dead-end / relevance kinds land when they get a construction site
}
```

A `Mark::Boundary` entry drives compaction entirely inside `project`:
projection folds every entry with `seq <= upto` to a summary user message.
There is no separate mutation of stored entries; the fold is a projection
concern, not a log mutation. See [consolidate.md](consolidate.md).

### `project_semantic` and the encoders

```rust
fn project_semantic(log: &[LogEntry]) -> Vec<WireTurn>
fn encode_messages(&[WireTurn], ImagePlacement, BreakpointPlacement) -> Vec<ApiMessage>
fn encode_responses(&[WireTurn], preamble: &str, PromptCachePolicy) -> Vec<InputItem>
fn project(log) -> Vec<ApiMessage>   // = encode_messages(project_semantic(log), defaults)
```

Projection is two stages. `project_semantic` (`src/project/semantic.rs`) is the
dialect-neutral fold: it decides **selection** — which entries reach the wire, the
compaction fold driven by a pre-pass over `Mark::Boundary` entries, result verdicts
(`Distilled` re-rendered from `keep` + `raw`, `Elided`, `TriageFailed`), pinned
notifications, surfaced context, and every audit-only exclusion — and renders what
every dialect sends identically (result text with its `[seq]` handle, and the
`source:` prefix opening a sourced `User` turn's first prose part). Its output is
a list of `WireTurn`s:

```rust
enum WireTurn {
    User(Vec<UserPart>),          // prose, tool results, fold summaries, pins — log order
    Assistant(AssistantOutput),   // one model turn, owned (a fork tail appends synthetic ones)
    Breakpoint,                   // a cache-write boundary requested here
}
enum UserPart { Text(String), ToolResult { call_id, seq, content, is_error, images }, Breakpoint }
```

Consecutive `ToolResult` entries coalesce into one `User` turn, and a `User` entry
that directly follows them merges into that *same* turn as `[result…, text]` rather
than forming a second consecutive user turn, so role-alternation and byte-stability
hold when a user turn is appended right after a turn's results (the seam-drain,
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
touching wire types: the agent loop appends one at the tail, a fork peek places one
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
{"format":"efference-session-log","version":1,"metadata":{…}}
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

`metadata` is the session boot recorded, in log-owned types
(`SessionMetadata`, `TriageModel`): the id and working directory, one
`LoggedInference` per role (main, spotter, and the triage selection when triage
does not share the main model, with the reasoning settings that inference is
called with), and the knobs and lineage a reader needs to describe the run —
temperature, effort, upstream sessions, `created_by`/`succeeds`, `autonomous`,
`keep_scratch`. These are deliberately **not** the live configuration or
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
(`view`, `wire`, `peek`, the TUI fold) would read that as one sequence with
duplicates in it — so the refusal is what keeps that unreachable. The session
directory is minted exclusively for the same reason.

Creation also takes the **writer lock** below, which the directory already
implies; it takes it so that every sink holds one.

### The writer lock

A log has one writer, including across host processes. `Log::create` and
`Log::open` each take an exclusive advisory lock (`File::try_lock`) on the file
and hold it for the life of the `Log` — the lock rides on the `File` the sink
owns, so it is released when the `Log` is dropped, after the last line is
flushed.

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

On the Responses dialect the marker is `prompt_cache_breakpoint: {"mode":"explicit"}`
on an `input_text` / `input_image` part, sent only when the model's cache policy is
`Explicit` (GPT-5.6 and later; the request also sets `prompt_cache_options.mode`).
Assistant items are not carriers, so the marker walks back to the last user part —
a bare-string `function_call_output` is lifted to its parts form to carry it — and
the physical marker can sit earlier than the logical frontier. Under any other
policy the encoder places nothing and the provider's implicit breakpoint governs.

The placements are the same seam `ImagePlacement` uses. A live session fixes
both choices at boot so its cached prefix cannot move under it; an offline
reader re-derives them from the recorded provider and model, so a log rendered
under a different current config still has the historical shape. The log does
not store either derived value. The consequence for the byte pins is that the
pinned bytes are pinned **per placement** — `project/test/cache.rs` covers the
arms and asserts the marker count stays one for each. On the Responses wire the fork peek's frontier has its own
pin (`distill/test.rs`): the peek's one marker sits on the last carrier before the
fork-local tail and never in it, and the next canonical request repeats the pre-fork
items content for content, marks its own tail, and carries the distillate rather
than the raw output or the peek's ask. The marker is a write instruction, not
content, so the two requests differ only in where it sits.

One request shape rides this marker everywhere: agent turns, fork peeks, and
keepalive pings send the same `tools[]`, `tool_choice: auto`, and thinking policy
([redaction-fork.md](redaction-fork.md), the shared envelope), so a session's
requests read and extend a **single cache lineage** rather than keying one per
request shape.

---

## The per-request usage record

Every upstream request appends one JSONL line to the session dir's `usage.jsonl`:
a wall-clock stamp, the request's origin (`turn` / `peek` / `triage` / `spot` /
`keepalive` / `aborted_attempt`), the upstream response id (Anthropic `msg_…`; an
OpenRouter-fronted request's generation id), the model the response named for
itself, and the four token counts. The main model and provider are fixed when
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
(the side channels carry their own prefixes at their own lineages), `peek`,
`keepalive`, and `aborted_attempt` stamp the touch clock the idle test reads
without counting, and any warm read clears it. An aborted attempt's zero
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

Two surfaces read it. `GET /state` publishes the live count as
`usage.cold_streak`, which the TUI header states beside the context meter once it
crosses the threshold and drops when a warm read arrives. `efference dev cache`
(docs/operating.md) replays a recorded `usage.jsonl` through the same `ColdStreak`
offline — per-origin read rates, the explained cold starts, and the longest run —
for one session, or `--all` for the corpus. It also reports the abort rate (aborted
attempts as a fraction of turn attempts): classifying `aborted_attempt` as its own
origin means those lines no longer read as unexplained cold turns in the report.

---

## The invariant tying caching to redaction

**Committed regions are immutable on the wire; all mutation happens in the
uncommitted tail past the last cache breakpoint.** Three operations must honor
this:

- **Redaction is at append.** Each tool result's `wire` is fixed the moment it
  enters the log and never changes — so no cached prefix entry is ever
  mutated. The invariant holds by construction.

- **Compaction folds a span.** Projection over a `Mark::Boundary` elides
  entries with `seq <= upto` and injects a summary, rewriting the prefix from
  the fold point. This is exactly one deliberate cache write; everything past
  the boundary is unaffected.

- **Ambient notifications** never splice behind a breakpoint. They queue
  out-of-band and are logged only at first-show. A pinned notification projects
  at the position of the `Triage` that pinned it — always tail-ward — so the
  wire grows only at the tail, never behind a cached block. Arrival entries
  (`Payload::Ambient`) project to nothing on the canonical wire. See
  [inbox.md](inbox.md).

Because redaction-at-append never touches a committed entry, no explicit freeze
primitive is needed yet. A `Commit` payload that freezes a boundary is
anticipated (see `Payload` above) but not built. Keeping the canonical lineage
lean — distilled, not raw — also keeps consecutive breakpoints inside the
20-block window longer.

---

## The wire is a pure projection; materializing it is pure memoization

`project` is a deterministic fold, so an in-memory materialized wire is a memo
with no semantic effect — drop it and rebuild byte-identically from the log.
Correctness can never depend on the memo.

Its invalidation boundary coincides with the prompt-cache breakpoint: every
tail append extends the wire only at the tail, and the one event that rewrites
the prefix — a `Mark::Boundary` fold — is exactly the "one deliberate cache
write." Incremental re-derivation follows the same prefix/tail boundary the
cache already imposes.

The memo is available as a pure-efficiency lever but is **never persisted** — a
persisted wire would become a rival source of truth. Tracked in the backlog
until the transcript is long enough to measure.
