# The TUI client

`efference tui` is the human-facing client. It is just-another-socket-client: it subscribes to the daemon's SSE streams over the same [control channel](control-channel.md) that `attach` uses, and it drives turns and approvals through the same HTTP verbs. The renderer is vendored-and-adapted from ferritin's interactive renderer into `src/tui/`. Ferritin already had the hard parts — a general-purpose document IR, a markdown→IR parser, syntect theming, a custom scrollbar, and line wrapping — and its render pipeline was domain-free (it renders an IR, not rustdoc), so the lift was strip-the-shell, not rewrite. Improvements flow back cleanly because the IR stays byte-identical to ferritin.

`efference gallery` (`tui/gallery.rs`) launches the real TUI on a fixed "one of each element" transcript with no daemon — a live design page that surfaces rendering gaps (literal `~~strike~~`, ordered-lists-as-bullets) the moment you look.

`efference view <session>` opens a **recorded** session the same way: the entries come from a session's `log.jsonl` (via `persist::load`, the same decode a live attach's entries arrive as) rather than from a daemon's event streams. The session is named — a session id under `.efference/`, a session directory, or a path to a log — and an unresolvable name lists the recent ones rather than opening an empty view. Both offline entry points share one runner (`tui/run.rs`'s `run_offline`), which seeds the event channel and holds every channel far-end for the session's life, so the UI never sees a closed channel where a daemon would be.


## Read-only: what a view drops

`efference view` is inert by construction, not by disabled buttons. `InteractiveState::viewing` holds the session's label and is what `read_only()` reports; four things follow from it, each because the thing it removes only exists relative to a running daemon:

- **No composer.** It reserves no rows (`render_frame`), so the transcript takes them. An input that cannot send is not chrome worth drawing.
- **No loop verbs on the [control strip](#the-control-strip).** The whole strip is the client-only tail — `theme t · help ? · close q`, with `detach` reading `close` because there is no daemon left running behind it. Pace goes with the rest: it is a control over a loop, and there isn't one.
- **No sidebar.** Every section of it (processes, stats, policy) is fed by the `state` frames of a live control stream, and there is no daemon to serve one.
- **The strip is the resting focus,** in place of the composer: focus starts there, `Esc` returns there, and `↓` off its bottom stops rather than falling into a composer that isn't rendered.

The log is read once at open, so a session that is still running shows as of the moment it was opened — a snapshot, not a tail. Reading a live session's log is allowed for that reason rather than refused; nothing in the view can write to it.


## The transcript is another `project(log)`

Just as the wire is a deterministic projection of the log into an Anthropic `messages` array (see [log-and-projection.md](log-and-projection.md)), `tui/projection.rs`'s

```rust
project(&[LogEntry], &LiveTail, &ViewState, Gate<'_>, pending_turns: &[String]) -> Document<'static>
```

is a *second* projection of the same log to a different target: a renderable tree instead of a model-facing message list. It reuses the wire projection's rules where they apply — distillate-else-raw for tool results, the streamed-vs-committed dedup (where the wire uses `streamed_*` flags, the TUI projection uses the structural `LiveTail`) — and diverges where a human reader differs from the model: nothing is elided or coalesced (the human sees the whole record), and the projection carries a deliberate **information architecture** — a typed entry per wire block, decorated by what it is, rather than a dump of labeled blocks.


## Information architecture: a flat sequence of entries

<a id="line-structured-text"></a>**Line-structured text vs. blocks.** An `Entry` separates its child *blocks* by a blank row, which is markdown block spacing and right for prose. Text that is already line-structured — streamed tool output, an arriving tool input, a multi-line pending turn — is not a sequence of blocks, and modelling one source line as one `Paragraph` doubled every newline the source actually had. `DocumentNode::Lines` is a run of rows with no separator, each wrapping like a paragraph; the live renderers use it. Thinking used to and no longer does: it is markdown, and goes through the markdown renderer inside a `Dimmed` wrapper (below).

The projection emits a **flat** sequence of `Entry { kind, marker, header, blocks, key }` — one per top-level thing on the wire, decorated for presentation. There is no conversational-turn container: a turn's thinking, its prose, and each call it made are separate entries, in wire order.

`EntryKind` is the taxonomy an operator actually scans by — **what I said**, **what the agent said**, **what the agent did** — rather than the wire's roles:

| kind | is | rendered |
|---|---|---|
| `UserMessage` | what I said | accent gutter down the left margin |
| `AssistantMessage` | what the agent said | the unmarked default voice |
| `Thinking` | what the agent thought | `✻ thinking`, dimmed register |
| `ToolCall` | what the agent did | `❯ tool · arg` |
| `ToolResult` | what came back | `✓`/`✗` and a size gloss, over the output |
| `Annotation` | what the *harness* says about a call | dim line, no marker |

A tool result occupies a `role: user` block on the wire, but it is the harness speaking rather than the operator, so it is its own kind and never carries the gutter. Authorship is **structural, not lexical**: you *see* who is speaking instead of reading a `▌ you` / `▌ assistant` noun.

A call and its result are **separate entries**, because they are separate blocks on the wire and a result is not part of a call's content. They read as one unit anyway: a `ToolResult` and an `Annotation` do not open a new visual group (`EntryKind::starts_group`), so no blank line separates a call from what the spotter made of it or from what came back.

```
❯ bash · echo hi
spotter cleared · reads only, inside the tree
✓ 2 lines
line one
line two
```

The result has no `result #N` / `[seq]` label — it is obviously the response to the call directly above it. Its header row carries the three things that have nowhere else to live: the `✓`/`✗` status (on the entry that actually failed, rather than on the call, which is a call regardless of how it went), the size of what follows, and the `· raw` flag when the distilled default is being overridden. It is also the focus target the raw/distilled verb hangs off.

Because all tools are known at compile time, results get **per-tool representations** instead of a raw JSON/text dump:

- `bash` — rendered as a shell code block; a single-line command goes inline on the call header, a multi-line command overflows into the call entry's body.
- `read` — strips the sentinel framing (`==<token> BEGIN <path>, LENGTH: n` … `END`) and highlights the file body by extension.
- `edit` / `undo_edit` / `retarget_edit` — the call header names the file and the operation (`edit · src/tui/composer.rs · replace`); the result's `===DIFF===` section parses into a color-coded `DocumentNode::Diff` (added/removed/hunk/context lines). The edit *input* is never dumped — the diff is the representation of what happened, and the `anchor` it used to show answered neither which file changed nor how.
- `write` — the path inline (plus `append`/`overwrite` when set), the `contents` as a code block highlighted by the path's extension. The JSON envelope rendered the file body as one escaped string with literal `\n`s, which is the least readable form of the only thing worth reading in the call. `call_args` takes `(name, &Value)` rather than a `ToolCall` so the live block renders through it too, from the settled half of a partly-arrived input.
- `read_image` — the path inline (plus `· long_edge N` when set; image tokens scale with area per docs/tools.md, so the requested resolution is part of what the call did). Mirrored in the web client's `callView`.
- Unrecognized tools — falls back to a plain code block of JSON input.

A **distilled** result is a case of its own. The wire renders a verdict as `@@ lines a-b @@` headers over the kept lines, because text is all it has to say "these came from there" with (see [the redaction fork](redaction-fork.md)); a terminal has a gutter, so the transcript renders the same lines in a `DocumentNode::NumberedCode` — each carrying its original line number, dimmed, with the gaps stated by the jump in the numbers instead of by a header row. Both views resolve the verdict through the one `project::distillate`, so they can differ in form but never in *which lines* they show — an operator comparing their view against the agent's is the whole point of the view.

**A tool with its own representation keeps it, over the triaged lines** (`distilled_body`). The tool-specific parse runs over the *whole* result and produces rows that each carry the raw content line they came from; the kept-line filter is then applied to those rows. So a distilled `read` is still framing-stripped and highlighted by extension, and a distilled edit's surviving lines are still classified as a colored diff — at whatever subset triage left. The one line mapping is what makes both possible at once: parse first (which needs the whole output), select second (which is expressed in raw line numbers). A `read`'s sentinel lines carry no row, so a verdict that kept one simply shows nothing for it; the numbers in the gutter are the *result's* line numbers, not the file's, because that is the coordinate system the verdict was expressed in.

The result header states the divergence in one place: `kept 3 of 40 lines`. A `Diff` node has no gutter, so a partial diff cannot state its own gaps — the header count is what says the view is partial there. When triage kept every line there is no divergence to state, so the header says nothing either: `kept n of n` names only that triage ran, and the head presents like any whole result.

The default view being the triaged one is a **reversal**, taken from live use: a large `read` distilled down to a few lines was rendering in full, and the difference between what was on screen and what the agent was working from was the thing the surface exists to show. The earlier position — that a subset of a diff is a *mangled* diff, not a smaller one, so rich-rendering tools should show the whole output and annotate the gap — is what the row mapping replaces: the rendering no longer has to be traded against the selection.

The distiller's **note** is not part of the result. It is that rung's account *of* the output, the way a spotter verdict is an account of a call, so it renders as its own `Annotation` entry beside the result rather than as a trailing line inside its box — where it read as something the tool had printed. The breadcrumb for lines the peek cap dropped before triage is a second such annotation. Both disappear when the result is flipped to raw: raw is what the tool actually returned.

A code block is highlighted, and carries its `❬lang❭` corner tag, **only when the language is known**. An untagged block is plain text in a bare border. The renderer used to default to `rust`, which was right in ferritin (a rustdoc fence is Rust unless it says otherwise) and wrong here, where the commonest untagged block is a tool result: `ls` output was labelled `❬rust❭` and coloured as Rust.

Content **wraps to the box's inner width** (`render_code_block`), through the same `wrap_points` break policy prose and the composer use — one break oracle, not a second wrap path. Wrapping is **linear in a line's length**: every width comparison in the policy is against something no wider than a row, so widths are measured only that far (`width_upto`) and never to the end of the text. Measuring the whole remainder once per row emitted — which is what it used to do — is quadratic, and a 16k-column line (a tool call's raw JSON input, a minified file, a base64 blob) cost ~300 ms per wrap in release, on every frame while it streamed. A line longer than the frame used to run past the right border and be clipped there, which made untruncated output worth much less than it looks: a stack trace or compiler error is exactly the shape that overflows. Row boundaries partition a line with no gaps, so each highlighter token is clipped to the row it appears on and nothing is dropped between rows.

The throughline: lean on pre-attentive cues (color, gutter, glyph) and tool-specific shape, not repeated labels.


## Nothing is truncated

**Every entry renders whole.** No line budget, no collapsed-by-default, no `(+N lines)` hint — the operator cannot perceive what the agent is dealing with through a summary of it ([positions.md](positions.md)), and that applies to a 400-line tool result and a twelve-paragraph thinking block alike.

This is a starting assumption rather than a permanent rule. What it replaced was a generic collapsible-block treatment with a shared 8-line budget, applied to a body built as *the call's argument overflow followed by the result*. A multi-line `bash` command therefore consumed the whole budget on its own and the result was not visible at all — the two things most worth reading, hidden by a default nobody chose for them. The lesson kept here: an on-demand fold, or a preference for what to fold, can be added once reading real sessions says *what* is reliably worth hiding. Deciding that in advance is what produced the defect.

Two consequences worth stating, since both look like omissions otherwise:

- **A focused entry usually has no verb.** Focus is an affordance in itself; `⏎` on an entry with an empty [block menu](#the-block-menu) does nothing, rather than reaching for a verb it doesn't have. Focusable-with-no-action is the ordinary state, not a gap.
- **There is one exception to "render whole," and it is not a truncation.** A *running* tool's output is unbounded and untriaged, so the live tail shows a **peek** — the last `PEEK_LINES` (10) lines, dimmed, under a `⋯ running · N lines, last 10` header. It is a glance at a live process, not a way to read its output; the whole output arrives, whole, when the call commits. A full-screen pager for a live process is future work.


## Result images

A tool result's images render in the transcript through the terminal's graphics protocol (`tui/image.rs`, `tui/render_image.rs`), via `ratatui-image`. The pieces and the invariants they hold:

**The node is a reference; the store holds the state.** `DocumentNode::Image` carries only the log address (`seq`, `idx` — the coordinates `GET /log/:seq/images/:idx` dereferences), the media type, and the pixel dimensions from the entry view's metadata. Everything stateful — fetch lifecycle, decoded pixels, per-protocol encodings — lives in an `ImageStore` on `InteractiveState`, keyed by that address. The split keeps the projection pure of the terminal's capability, and keeps `DocumentNode`'s derives (a `Protocol` implements neither `Debug` nor `PartialEq`).

**The reserved rectangle is a pure function of metadata and render width** (`ImageStore::footprint`) — never of whether bytes have arrived, decoded, or failed — so byte arrival repaints the reserved rows and reflows nothing. A test pins the document height as identical between the in-flight and landed states. The natural size (pixels over the queried font size) rounds *down*: a ceil'd final row is mostly aspect-fit padding, which iTerm2 composites as an opaque band. The footprint caps at 24 rows — sizing, not truncation; the fit scales, nothing is cut. Residual sub-cell padding is encoded in the theme's background color (`set_theme_background`, re-applied on theme change with the encodings dropped), so it disappears into the transcript under protocols that render the default transparent pad opaquely.

**Two byte sources, one render path.** A live entry view omits `data` (docs/control-api.md), so `handle_event` marks the slot fetching and hands the address to a tokio-side fetcher (`Client::log_image` over the socket), which decodes off the UI thread and answers with a `TuiEvent::Image`; a failure lands too, and the reserved rows carry the reason. An offline log (`efference view`, the gallery) delivers the base64 inline, decoded at the event fold. Slot presence gates both, so a reattach replay resolves nothing twice.

**Encoding is exactly-once per (image, footprint)**: the detected protocol's escape for the fully-visible paint, plus a halfblocks scratch buffer — ordinary cells, rendered once through the real widget — for everything the escape can't do. A resize re-encodes at the new fit; nothing re-encodes per frame.

**The paint rule** (`render_image_node`): fully visible → the native protocol's escape (the widget's cell-skip keeps ratatui from overwriting it); partially visible, top *or* bottom → a row-slice of the halfblocks scratch, blitted cell-by-cell — the pixel protocols paint only from the image's top edge, and iTerm2 clips not at all, so cells are the one representation any sub-range of rows has. While the viewport is *moving* (`layout.scroll_in_motion`, set by `render_frame` comparing the settled offset against the last painted one), a protocol with no stored-image concept (iTerm2's OSC 1337, sixel — every reposition retransmits and re-decodes the payload, which flashes) holds the halfblocks slice; a follow-up settled repaint, scheduled by leaving `dirty` set, restores the crisp render. Kitty stores and re-places, so it stays native through motion.

**Detection is a startup act.** `Picker::from_query_stdio()` runs in `ui_loop` after raw mode and before the crossterm reader thread takes stdin. `--images auto|kitty|iterm2|off` (and `EFFERENCE_IMAGES`) overrides it in both directions, because tmux makes detection lie both ways. On auto, a terminal whose environment identifies iTerm2 (`TERM_PROGRAM`/`LC_TERMINAL`) always gets OSC 1337: iTerm2 answers the capability queries above its own weight class — DA1 advertises sixel, 3.5+ answers the kitty query partially — and renders the query-ranked winners as garbage (backlog/tui-image-sixel.md). With images off (or no capability), an image renders as its one-line reference, no reservation. The resolved protocol and font size print on the status line at launch.

**Snapshots pin halfblocks** (`Picker::halfblocks()`, the deterministic constructor — `Screenplay::halfblock_images`): the mosaic is ordinary cells, so the fiddliest part — the partial-scroll arithmetic — is grid-snapshot-testable, while the protocol modes stuff escape blobs into cell symbols and stay out of snapshots. The gallery carries a generated test card (hue sweep for color depth, a circle that turns elliptical under a wrong font-size answer) with its bytes inline, exercising decode → encode → paint under whatever protocol the real terminal answers.


## View state: surviving re-projection

The transcript re-projects on every streamed token, and any entry can be re-projected at any time (a result or verdict arriving rebuilds the entry that made the call). That means a raw-vs-wire toggle cannot live inside the rebuilt `Document` — it would reset mid-stream. It lives instead in `tui::state` as a `projection::ViewState` keyed by `call_id`, and the projection reads that state when building each result. `ViewState` also records which `call_id`s were toggled, since a view flip is not a log event and the incremental projection has no other way to learn of it.

```rust
struct ViewState {
    raw: HashSet<String>,   // call_ids flipped to raw output
}
```

One toggle, where there were two: expand/collapse went with the truncation. The raw fork survives because it is a choice between two real forms of the content, not a choice about how much of one to show. It flips a focused tool result between its distilled wire form and its full raw output; both are already client-side from the log entry's `WireState` and `RawResult` (see [the redaction fork](redaction-fork.md)), so this is a pure render choice with no round-trip. When showing raw over a distillate, the result header gains a `· raw` annotation so the human knows they are off the default view.


## The live tail

In-flight streaming deltas not yet committed to the log live in a `LiveTail`:

```rust
struct LiveTail {
    thinking: String,             // extended thinking streamed so far this turn
    assistant: String,            // in-flight prose this turn
    tools: Vec<(String, String)>, // (call_id, accumulated output)
    phase: Option<Phase>,         // CallingTool { name, input }
}
```

**Thinking accumulates, it does not flash past.** Streamed thinking is live-tail *content*, beside the assistant prose buffer — not a phase. It was a phase once, and a phase is a single slot, so `tool_call_start` overwrote it: a turn that thought briefly and then called a tool showed its reasoning for a few frames and lost it until the turn committed, which is most turns. It now stays put while the model moves on to prose or a call, and the committed block supersedes it.

Live and committed thinking are built by **one renderer** (`nodes::thinking_block`), rendered as a `Thinking` entry ahead of the turn's prose in the dimmed register — thinking is never the answer and must not read as it. A test asserts the two produce byte-identical transcript rows for the same text: two renderers would drift, and the drift surfaces as the transcript visibly rewriting itself at the moment of commit, which reads as a bug even when both renders are correct on their own.

**Thinking is markdown**, through the same `MarkdownRenderer` assistant prose uses. Nothing requires it to be, any more than prose is required to be; it simply is — 73.5% of 653 blocks across 20 sessions carry an explicit markdown construct ([web-client.md](web-client.md) holds the measurement, which the TUI inherited rather than re-derived). Rendering it as literal lines left fences, inline code, emphasis, and lists as the characters that spell them. This also *resolves* the regression that preformatting was defending against: reflowing `- a\n- b` into `- a - b` stops being possible once the list is a list.

**The dimmed register** is what markdown-rendering thinking required. The web client gets muting from a CSS class that cascades over whatever the renderer produced; the TUI has no cascade, so `DocumentNode::Dimmed` wraps a subtree and `render_node` sets a flag its style resolution consults (`span_style.rs`). The register is a property of *where* a node sits, not of what a span means, which is why it wraps rather than adding a dim variant per `SpanStyle`.

Inside it, **prose dims flat and code keeps its hue**, blended toward the register rather than replaced by it (`CODE_DIM_BLEND`). The tension is real and this is the chosen point on it: dimming a code block the whole way makes it legible as structure and useless as code, which is the visible reason to render thinking as markdown at all; leaving it at full strength weakens the never-mistaken-for-the-answer property exactly where a reader's eye lands. Blending keeps keyword/type/function separation while the block still sits below the answer's brightness. The colors are invisible in the no-color snapshot grid, so this is a **pixel** question — `efference gallery` carries a thinking block with a fenced Rust block for that purpose.

It is shown **in full**, and keeps the model's own lines. Reasoning streams past too fast to read at generation speed, so the committed copy is the one a human actually reads, at their own pace — which it can only be if all of it is here. It is also not flowing prose: it holds lists, indentation, and snippets, so reflowing it to paragraphs turned `- a\n- b` into `- a - b`. (It had to, once: one node per line was one *block* per line, and an entry separates blocks by a blank row, so the whole entry double-spaced. `DocumentNode::Lines` renders a run of lines without that — see [Line-structured text](#line-structured-text).) Encrypted (`RedactedThinking`) blocks render a line saying so rather than nothing, so the record never implies the model didn't think when the provider merely withheld the text.

`project` renders the committed log first, then appends the live tail — thinking first, then in-flight assistant prose as its own entry, then each running tool as an output peek. When a committed `LogEntry` arrives, `LiveTail::commit` clears the superseded buffer (assistant prose on `Payload::Assistant`, tool output on `Payload::ToolResult`), mirroring the wire projection's `streamed_*` dedup flags, now structural.

**The live phase: a turn that produces no prose is still visibly working.** The streaming sink is typed (`api::Streamed` — `Text` / `Thinking` / `ToolStart` / `ToolInput`), so thinking and a tool call's arriving input reach the client as their own `output` frames (`thinking_delta`, `tool_call_start`, `tool_call_input`) rather than not at all. Thinking accumulates (above); a call in progress is the one remaining `phase`, rendered `❯ calling <tool> · <arguments>…` as its input arrives. Three rules:

- **In full, never truncated.** The point is to watch the work as it happens, so everything that has arrived is shown.
- **Parsed as a prefix, and what may be claimed from it is decided positionally.** A tool call's input mid-stream is a *prefix* of a JSON document, and it is read as one (`jiter`, `PartialMode`): a loose parse keeps the trailing incomplete value, a strict parse drops it, and the one key the first has and the second lacks is exactly the value still in flight. Within an object **only the last key can be incomplete** — the bytes for the earlier ones arrived and closed — so the settled pairs may back an identity claim and the in-flight one may not. `{"path":"src/api.rs","contents":"use st` renders `write src/api.rs` (true — `path` closed) with the contents as arriving content; a half-arrived `"src/api.r` stays out, because `write src/api.r` is well-formed, plausible, and false. Before any pair closes there is nothing to claim and the raw bytes are shown, which claim nothing. See `projection/partial.rs`.
- **One renderer with the committed call.** The settled arguments go through `call_args`, which a committed call also renders through, so the header *fills in* as bytes land rather than changing shape at commit. The in-flight value never reaches it. A string value with newlines in it renders as a code block in the language of whatever settled path names it, which is why the identifying argument arriving first is what makes this worth doing — and is why the streamed contents must be *decoded* (a raw JSON input is a single line, its newlines `\n` escapes).
- **Prose supersedes the phase**, and a committed turn clears it: prose is its own evidence of progress, and the committed entry is the record — which the transcript keeps (below). Thinking is no longer superseded by either.

Without this the window between submitting a turn and the first prose token was blank, and blank is indistinguishable from a hung harness — a risk of interrupting or abandoning a live session on false signal. The daemon's own log has the matching fix: it prints `(generating — turn N)` before blocking on the model, so its last line is no longer a stale `idle`, which reads as *finished*.


## Pending user turns: the delivery indicator

A submitted turn is not lost while a trajectory runs. `submit_composer` pushes the text onto an ordered `pending_turns` queue and reprojects at once; `project` renders each as a **dimmed, `⋯`-marked user turn** after the live tail — the optimistic "handed to the daemon, not yet on the wire" echo. When the matching `User` `LogEntry` streams back via `/events` (that entry *is* `project(log)`, i.e. now on the wire), `handle_event` drops it from the queue by content match, and it re-renders as a normal committed turn. The pending→committed flip is a faithful delivery signal, not cosmetic: it mirrors, on the operator's side, the daemon's mid-trajectory seam-drain ([operator participation](control-channel.md#operator-participation--the-intervention-points), [control channel](control-channel.md)).


## The pace indicator

The operator's pace ([operator participation](control-channel.md#operator-participation--the-intervention-points)) arrives on the `pace` state topic, decoded into a `PaceView` and surfaced **loud** in the status bar: a `⏸ HELD` / `⏱ 10s` indicator ahead of the status text whenever the pace is non-default (the default fast-and-unheld state adds no chrome). It is status-bar chrome, never transcript content — pacing never reaches the log or the wire. The transcript keys `+`/`-` step the speed dial along a coarse ladder and `p` toggles the hold, each optimistically updating the local `PaceView` on the keypress (the topic's next frame reconciles) and issuing `PATCH /loop {pace}` over a dedicated pace channel. Because a slow pace *is* the window during which the operator has room to read and type, pacing and the mid-trajectory injection compose: the sleep is exactly when a message can be interjected.

The `upstream_failure` topic carries an optional upstream-failure diagnostic. A non-2xx response renders as `upstream 429 ×2` ahead of the status text; the count covers consecutive matching statuses. The daemon clears the topic on the next 2xx, so the indicator disappears on recovery rather than persisting as a survived failure. It is never transcript content.

The `upstream_retry` topic carries the retry in flight ([transport.md](transport.md)), decoded into an `UpstreamRetryView` and rendered ahead of the failure indicator — it is the live state, and it says what is being done about the status the failure indicator names. The daemon's own lead line is shown verbatim after a `↻`, so the status code is never the headline: `↻ stream aborted, retrying (2 of 12)`. A fork peek's retry takes a `peek:` prefix, since an exhausted peek withholds a verdict where an exhausted turn ends the trajectory. The topic is level-triggered — `null` is the all-clear and removes the indicator. The status bar renders no countdown: the topic's `next_attempt_at` and the upstream's `detail` text are read past, because the bar does not tick and the error body belongs in the log.

## Graduated Esc


`Esc` is a **state machine, not a press-count** ([operator participation](control-channel.md#operator-participation--the-intervention-points)): each press's effect is a pure function of the current state — so there is no counter, no timing debounce, and nothing to reset. Precedence, gentlest first (`handle_escape` in `tui/state.rs`):

0. **an inline sub-state is open** (the pace [widget](#the-control-strip), or a [stager](#the-control-strip) editing an argument) → revert/cancel it, restoring the value or draft at entry. These previews were never committed, so this only undoes local state.
1. **not at rest in the composer** (an overlay is open, or focus is in the transcript) → return to the composer (`Esc` is one-directional toward the composer).
2. **at the composer, held** → **interrupt** the in-flight tool (`POST /loop/interrupt` — the loop stops at the next juncture and the running call's output is withheld from the model, plus, on a foreground bash, its process group is killed; see [control-channel.md](control-channel.md). Mid-tool is the one point pacing can't reach).
3. **at the composer, not held, loop active** → **pause** (set the `held` overlay via `PATCH /loop {pace:{held:true}}`; the loop stops at the next between-step juncture).
4. **at the composer, not held, nothing happening** → **swallow** (a silent no-op).

So sitting in the composer while the agent works, *one* `Esc` pauses and a second interrupts — no wasted "return to composer" press; and jamming `Esc` from anywhere still walks return → pause → interrupt. "Loop active" is read from `/state`'s **`running`** flag (unioned with the streaming `pending_request`), which — unlike streaming deltas — stays true through a *silent* long-running tool, so `Esc` can still reach a runaway command. `Esc` only ever *enters* the hold or *fires* the interrupt; resume is the pace layer's job (send a message — submit always resumes — or the `p` key). The hold and interrupt verbs are exactly unit 2's `/pace` and the long-standing `/interrupt`, so this is a TUI-only key binding over existing daemon affordances.

Alongside kill, transcript-navigation `b` fires the **foreground→background handoff** (`PATCH /processes/:id {background: true}`, [supervisor.md](supervisor.md)): the in-flight tool keeps running as a `p#<n>` process instead of being killed or waited out. When the loop is idle the key is a status-line no-op (nothing sent). There is no separate note-capture flow — a message typed after `b` rides the seam-drain to the model alongside the handoff result. Both intents share one channel to the tokio side (`frontend::Interrupt`: `Kill` → the loop interrupt, `Detach` → the process patch, whose foreground-id resolution the client verb owns).

> **Discoverability.** Pause is reachable where the operator lives — `Esc` in the composer — and the [control strip](#the-control-strip) now shows the pace, handoff, and kill verbs as visible, focusable items one `↑` from the composer. The per-entry raw verb is on the [block menu](#the-block-menu). The single-letter transcript verb binds (`p`/`+`/`-`/`b`/`r`/`t`) have been purged (backlog/tui-control-strip/): the verbs now answer only on the strip (as scoped mnemonics or clicks) and the block menu, and a plain letter in the transcript no-ops.


## Wiring: two threads, one crossbeam channel

crossterm's event read blocks, so the UI runs on a dedicated OS thread (`tui::ui_loop`). A tokio task (`tui::bridge`) holds one connection to the unified control stream (`GET /events?include=log,output,state`, [control-api](control-api.md)) over the [transport](transport.md), decodes its frames — durable `LogEntry` entries, ephemeral `assistant_delta` / `tool_output` deltas, and whole-topic `state` replacements — into `TuiEvent`s, and forwards them over a crossbeam channel into the UI thread. There is no poll: the connect snapshot seeds every topic, and the daemon pushes a replacement when one changes. A 1 Hz clock tick in the UI loop advances the elapsed-time displays (process running/idle seconds, seconds since the last request), which derive from absolute stamps because the wire deliberately carries no ticking values.

The UI loop drains arriving events through `InteractiveState::handle_event` and sets a `reproject_pending` flag rather than re-projecting per event. Before each paint it calls `flush_reproject`, which brings the transcript up to date once and clears the flag. A reattach replay that delivers N entries therefore costs O(N) event folds and O(1) projections, not O(N²).

**Terminal input folds the same way.** A wheel notch or a held key delivers a burst of crossterm events, and the reader thread keeps queueing while the loop paints — so painting per event makes fast scrolling lag behind the wheel by however far the queue has run ahead. The loop applies the whole burst, then paints once. Two events stop the fold: a quit, and a click, which the paint has to consume before another can be recorded in its place.


## What a frame costs

A streaming session repaints ~33 times a second against a document that only grows, so the property to hold is that **a frame costs the viewport, not the log**. Three mechanisms, each with a measured reason:

**The transcript is a materialized view, not a rebuild** (`tui/projection/incremental.rs`). `project` — the from-scratch fold above — stays the reference definition and the thing tests compare against, but `Transcript::update` keeps the document between repaints and replaces only what changed. It rests on `entry_nodes` being independent per entry: a committed entry's nodes are a function of that entry, the annotations indexed against the calls it makes, and the human's per-call view toggles — never of its neighbours. So the invalidation rule is one sentence:

> A committed entry is re-projected only when a call it owns changes.

Everything that can change about an already-committed entry arrives keyed by `call_id` — a tool result, a spotter verdict, the gate moving, a raw toggle — so each marks one owner dirty (`ViewState` records its own toggles for this, since a view flip is not a log event). The uncommitted tail is rebuilt every time; it is small and changes constantly. A payload that annotates a call in a new way joins the list; `invalidate_all` is the always-correct fallback.

**Layout is cached per top-level node** (`DocumentState::layouts`, keyed by render width). A node is laid out when it is *visible* or when its cached layout is missing; otherwise its height advances the cursor and its recorded actions are replayed. The cache lives beside the nodes and moves with them — `splice_nodes` is the only way the document changes, so a replaced node can never keep a stale height. The nodes above the viewport still have to be *walked* (their heights are what says where the viewport starts) but that walk is integer arithmetic, not re-wrapping. Off-screen rows already skipped their cell writes and their syntax highlighting; what they were still paying was span wrapping, and this is what stops that.

**A scroll knows where the bottom is.** `max_scroll` reads the cached layout, and a re-projection drops that cache — while the event loop re-projects *before* draining an input burst, so during a stream a scroll event essentially always arrives with no valid layout. Answering `0` there (the obvious "unknown" default) made every scroll evaluate `offset >= max_scroll` as true, i.e. "the operator is at the bottom", which silently re-armed stream-following and snapped the viewport to the end from wherever they were reading. The last completed walk's document height survives the invalidation and backs `max_scroll` instead. It errs the safe way: content only ever arrives *below*, so it under-estimates the bottom rather than falsely claiming to be at it.

**Measure, then paint — once.** `render_frame` runs the walk in `measuring` mode first (`row_visible` is false for every row, so nothing is written and no highlighting runs), which fills in any missing heights and yields the document height. A stream-following viewport is then pinned to the new bottom *before* the paint, so there is no stale-offset frame to correct with a second full draw.

**Syntax highlighting is computed once per block, not once per frame** (`render_code_block`'s `HighlightMemo`). The layout cache is per *top-level node*, and a multi-screen code block is one node — so while any part of it is on screen it is re-laid-out on every paint, and re-parsing it each time is what made scrolling past a long highlighted block drag. Worse, syntect carries parse state line to line, so a block cannot be coloured from its middle: the cost peaked exactly where the operator had scrolled furthest into it. Colours are a pure function of (theme, language, text), so they are memoized under a hash of those three — a theme change or an edited block misses and recomputes rather than showing stale colour — bounded by a segment cap and evicted oldest-first. Measured on a 2000-line highlighted block, debug build: 1.57 s a frame before, 8.4 ms after.

**A block that is still growing continues from the last frame's state** rather than missing the memo outright. The memo is keyed on the block's *text*, so a block whose text changes every frame — a tool call's contents arriving — misses every frame and re-parses from line 1: the very cost the memo exists to prevent, reached through a door it cannot see. Measured at 401 lines re-parsed per frame for a 400-line block, growing with the file and worst at the end of the write, where the operator is still watching. The text is append-only though, and syntect exposes its per-line state (`HighlightLines::state` / `from_state`), so the previous frame's work is reusable: the state as of the last **complete** line is kept, and only the lines that arrived since are parsed (401 → 1, flat in block size). The trailing partial line is recomputed each frame from that state and never committed to it — it can mis-lex until its newline lands (an unterminated string literal colours the rest of the line as a string), and it corrects itself when the line closes. Anything that is not an append — a different block, a theme change, an edit — starts over, so correctness never rests on the continuation being the right guess. A test asserts that every prefix of a streamed block colours *identically* to a cold pass over that same prefix, checked at every byte offset.

`document.laid_out` — how many nodes the last walk actually laid out — and `document.highlighted` — how many source lines it actually ran syntect over — are what the cost tests in `harness/test/render_cost.rs` assert on: an 8× longer log must not lay out more nodes for one streamed token, a repaint or a scroll must highlight *nothing*, and a streaming write must colour only the lines that arrived. Those are the regression guards, in place of timing tests.

**Wall-clock, when a count isn't enough.** Those counts are stable and testable, which is why the tests assert on them, but they say nothing about whether a frame fits the 30 ms paint tick on a real terminal. `--frame-cost` (on `tui` and `gallery`, `frame_cost.rs`) times each paint and each re-projection separately — they are different fixes when one dominates: the projection rebuilds the live nodes, the paint lays out what they became. The status bar carries p50/p95/max over a rolling window beside the `laid`/`hl` counts, and the session's distribution prints on exit, including how many frames ran over the tick. Off by default, and unreachable from the snapshot harness, since a frame time cannot match twice.

**Driving the animation-rate behaviours.** `efference gallery --stream` plays a scripted turn — thinking, then a long file write whose JSON arrives fragment by fragment, then a command whose output streams — over the offline gallery, through the real `ui_loop` with no daemon behind it (`gallery/stream.rs`). `--chunk`/`--interval` set the pace, so both a real-model cadence and fragments arriving faster than the paint tick can be provoked. Snapshots pin layout; this is for what only exists in motion.


## The composer

`tui/composer.rs` is a dependency-free, emacs-keybind multiline editor. Its interface is deliberately swappable: `handle_key` / `render` / `take` (or `clear`). The buffer is one UTF-8 string with byte offsets for each logical-line start. The cursor, edit ranges, and visual-row boundaries are extended-grapheme boundaries; grapheme counts are derived for vertical goal-column motion. This lets combining marks, variation selectors, ZWJ emoji, and regional-indicator flags move and delete as one unit. The composer shares `render_span::wrap_points` with the document renderer; its width and hard-break traversal measure grapheme clusters with `unicode-width`, although an individual terminal can use a different emoji cell width.

`tui-textarea` was the intended first cut but is pinned to an older ratatui/unicode-width ecosystem and does not resolve against ratatui 0.30, hence the hand-rolled editor. The narrow interface makes replacing it later surgical.

**Selection and clipboard.** The composer tracks an optional anchor alongside its cursor; a non-empty span between them is rendered with the theme selection style. Shift-modified character, word, line, and vertical movement extends from the anchor; unmodified motion collapses to the span's directional edge, and `C-x h` selects the buffer. Typing, newline, tab, deletion, and kill commands replace or delete a selection before their ordinary operation. Mouse press, Shift-click, and primary-button drag use the same wrapped visual rows as rendering, mapping positions only to grapheme boundaries.

`M-w` copies a selection, `C-w` cuts it (or deletes the preceding word when there is no selection), and `C-y` pastes. `Super-C`/`Super-X`/`Super-V` are aliases when the terminal reports `SUPER`. `tui/clipboard.rs` owns the persistent `arboard` system clipboard and an in-process text fallback; the composer itself receives and returns plain text only. Copy and cut update the fallback before an attempted system write. Paste refreshes it after a successful system read, or uses it after an access failure. In arboard 3.6, `ContentNotAvailable` does not distinguish empty clipboard text from an unsupported format, so it neither uses stale fallback content nor labels the content type.

**Undo and redo.** `C-/` and `C-_` undo, and `M-_` redoes. When the terminal reports `SUPER`, `Super-Z` and `Super-Shift-Z` are undo and redo aliases. History is a 200-entry bounded timeline of whole-buffer snapshots, including cursor, selection anchor, and goal column. Adjacent ordinary typing joins a burst until 750 ms of idle time, while contiguous Backspace and forward Delete runs coalesce separately. Newline, tab, paste, selection replacement, cut, word deletion, and line-kill commands each form one operation. A new edit after undo discards redo; external draft replacement and clearing reset history.

The UI requests crossterm's `DISAMBIGUATE_ESCAPE_CODES` keyboard enhancement at startup and pops it during normal and panic terminal restoration. Kitty-protocol terminals that support the enhancement can report `SUPER`; terminals that do not continue emitting ordinary key events.

**Focus model.** Focus lives on one vertical axis — transcript, [control strip](#the-control-strip), composer — and the same physical key takes a context-dependent meaning (`C-p` is "previous line" composing, "previous unit" navigating), the way an editor's motion keys don't stop existing when you leave the buffer. There is no modal toggle: you cross between contexts by *moving*. `↑` at the top of the composer (the `0,0` detent: one press lands at the start of the first line, the next crosses the edge) spills focus onto the control strip, and `↑` again into the transcript; `↓` reverses — past the last unit it stops at the strip, then falls back into the composer; `Esc` is a graduated *return to steady state* (see [Graduated Esc](#graduated-esc) below) whose first rung returns to the composer from any stop. In the transcript context every entry is a focus **unit** — each message, each thinking block, each call, each result — and the focused unit is marked by an accent glyph **`▌`** down its full height (a glyph, not reversed-video, so it shows on any theme and in the snapshot grid). `↑/↓` (or `C-p`/`C-n`) move between units; `←`/`→` choose among the focused unit's [block-menu](#the-block-menu) verbs and `Enter` fires the selected one — on most entries there are none, and `Enter` is inert. The verb keys — `r` raw, `t` theme, `+`/`-` pace, `p` pause, `b` background — are **not** bound here: after the bind purge (backlog/tui-control-strip/) each lives where it is shown (raw on the block menu, the rest on the strip), so a plain letter in the transcript is a clean no-op. The surviving letter binds are navigation and utility only (`g`/`G` top/bottom, `m` mouse, `?`/`h` help, `q` quit); typing never leaks into the composer. Submit in the composer drives `POST /turns`; submit while a gate is pending resolves the gate (below); `C-d` on an empty composer quits (shell EOF).

Beside this axis sits a second *column*: the [sidebar](#the-sidebar). `Tab` in command mode (transcript or strip focus) toggles focus onto it and back to where focus left; `Esc` still homes to the composer from there. In the composer `Tab` is a typing key — a tab enters the draft as the four spaces every render path displays it as — so entering the sidebar never steals a key the composer wants.


## The control strip

A one-row surface between the transcript and the composer (`tui/control_strip.rs`), always present and never empty: the currently relevant **loop verbs**, rendered as focusable, clickable, button-like items. It is the discoverable slow path and the keybinding legend in one place — each item shows the key that fires the same verb without visiting the strip.

Contents are **one relevance-ordered list per state** — the state's context verbs, then an omnipresent tail — filled to the available width and cut from the right. So `↑ ⏎` from the composer always lands on the most relevant verb, and what a narrow terminal loses is always the least relevant one. Context verbs by state:

| state | context verbs (most → least relevant) |
|---|---|
| idle | *(none; `resume` when held)* |
| gate pending | `approve ⏎ · allow… · abort`, with a dim `type below to redirect` note |
| running — foreground `bash` | `background b · kill ⎋⎋ · pause ⎋` |
| running — any other tool, or still generating | `interrupt ⎋⎋ · pause ⎋` |

`held` is a modifier rather than a state of its own: wherever the hold verb appears it reads `resume p`. A hold set mid-trajectory outlives the turn, so `resume` stays reachable at idle; there is nothing to pause when nothing is running.

The tail is `pace <v> · theme t · help ? · detach q`, dimmer than the context verbs and always last — which makes it exactly what the cut takes first, right to left. Every tail verb has a keyboard fallback, and the order protects `pace` most (it is a control, not a setting) and `detach` least. `detach` leaves the TUI while the daemon and session keep running; there is no separate terminate verb, because leaving the client is the only thing either would do.

Only verbs that apply in the state are shown: the strip flashing between states is preferred to carrying an affordance that would do nothing. `background` is **foreground-`bash`-only** for that reason — the handoff adopts a live child into the supervisor, and there is no child to adopt anywhere else.

`kill` and `interrupt` are **one verb** (`StripVerb::Interrupt`, `POST /loop/interrupt`) under two labels, because it keeps two promises and only one of them is universal. Everywhere, it stops the loop at the next juncture and withholds the in-flight call's output from the model ([control-channel.md](control-channel.md)). On a foreground `bash` it *additionally* kills the process group — the stronger promise, and the one worth naming, so there it reads `kill`. Elsewhere — an MCP tool mid-call, or the model still generating — it reads `interrupt`. It is shown in every active state: before the latch it would have been a button that did nothing outside bash (nothing awaits the abort future during a synchronous MCP call), which is why the strip previously offered no interruption verb there at all.

Two transient sub-states own the row outright and carry no tail: staging (no items at all — the row is the staging note) and the active pace stepper (only the loud `▴ pace <v> ▾` and its note). Both are described under item classes below.

Because the item list is a pure function of the state topics plus the committed log, re-ranking happens only at state transitions — never while the strip sits stable.

**Which tool is running** comes from the log the TUI already holds, with no daemon round-trip: the loop appends the assistant entry *before* running its call and the `tool_result` after (`src/agent.rs`), so the last call with no paired result is what is executing now (`in_flight_call` in `tui/state.rs`). `disable_parallel_tool_use` means at most one call per turn, so that tail is the whole answer.

The focused item fills a reserved marker cell with `▸` (a space when unfocused), so focus movement never shifts text and stays visible in the character-grid snapshots; the raised-surface color change is what pixels show, asserted in tests via `cell()`. `←`/`→` (or `C-b`/`C-f`) move between items, `⏎`/space or a mouse click activates — clicks resolve against per-frame recorded column ranges (`strip_hits`). Activation dispatches onto the same mechanisms the keybindings use (the pace and interrupt channels, gate approval); `detach` raises a quit flag the UI loop checks alongside the key handler's return, since clicks have no quit return path. A state's dim note has its width reserved out of the item budget before any item is placed — an instruction is never what the cut drops — and is then drawn adjacent to the last item that fit. The strip is chrome: never projected, never in the log.

**Scoped mnemonics.** While the strip is focused, a plain letter that matches a *shown* item's hint fires that item (`b` → background while a foreground command runs, `t` → theme, `p` → resume while held), wherever the selection currently sits — a shortcut, not a focus move. This is the only surviving letter-form of the purged verbs (backlog/tui-control-strip/): the letters answer only where their item is on the strip, never as buried global binds, so `p` does nothing from an unheld strip (which shows no resume) and pausing a *running* loop is Esc, matching graduated Esc. Items whose hint isn't a bare letter (`⎋`, or `allow…`'s empty hint) are driven by their own key or the arrows. An unmatched key falls through to the remaining transcript keys (scroll, help, quit).

**Item classes.** Most items are *buttons* — `⏎` fires the verb. `allow…` is a *stager*: a verb that takes an argument, edited in the composer. Activating it parks the operator's draft (`Staging` in `tui/state.rs`), pre-fills the derived grant spec — the bash program for a `bash` gate, empty otherwise (bare `.allow` derives a tool grant at commit) — and swaps the prompt to the verb's chip (`allow › rm█`, drawn by `render_frame` left of an untouched composer), with a dim `draft saved · ⏎ commit · ⎋ cancel` note on the strip row. `⏎` commits the edited spec through the same `.allow` submit path the typed form uses, then restores the parked draft; `Esc` cancels and restores — the new gentlest rung of graduated Esc, checked before any return/pause/interrupt step. While staging, the gate verbs leave the strip (the composer owns the interaction — a lingering `approve ⏎` would misstate what `⏎` does, and clicking it would resolve the gate out from under the stager); a gate that resolves elsewhere (another client, a daemon-side abort) likewise cancels the staging rather than letting its commit fall through to a normal turn submit. The third class is a *widget*: a verb whose value is adjusted in place. The tail's `pace` item is one — `⏎` enters an inline stepper (`PaceWidget` in `tui/state.rs`), rendered loud as `▴ pace <v> ▾` (the `▴`/`▾` are the grid-visible "stepping" tell, the way `▸` marks focus), with a dim `↑/↓ adjust · ⏎ set · ⎋ revert` note. While active, the stepper owns the row — every other item leaves, so nothing else can be focused or reflow behind the changing value — and it alone captures `↑`/`↓` (`↑` climbs toward more between-turns delay). Each step **previews** by mutating the live pace (the status bar tracks it) without pushing to the daemon; `⏎` commits the previewed speed over the pace channel and leaves, and `Esc` reverts to the speed at entry — the same gentlest rung of graduated Esc as a stager cancel, and since previews were never sent there is nothing to unsend. It replaces the earlier `slower`/`faster` buttons, so between-turns pace is adjusted only here — and since `pace` is in the tail, it is reachable in every state.


## The block menu

The strip carries *loop verbs* (act on the daemon, matter mid-composition); the **block menu** (`tui/block_menu.rs`) carries the *entry verbs* of one transcript unit — the actions on the entry itself. It travels with `↑`/`↓` focus and renders trailing the focused unit's header line (`✓ 20 lines  ▸ raw`), styled like the strip's items: a reserved marker cell (`▸` selected, a space otherwise, so moving the selection never shifts text) and the same button surface. Trailing the header rather than taking its own row means moving focus never reflows the transcript — a dedicated row would repaint on every `j`/`k`.

The verbs a unit carries are a pure function of the entry (`block_menu::menu_items`), computed once and read by two callers with no drift: the renderer (which has the node in hand) and the key/mouse dispatch (which looks it up by focus). There is exactly one verb so far:

- **raw / distilled** — the `toggle_raw` flip (docs/redaction-fork.md), on a keyed `ToolResult`. The new home for the forgettable `r`.

**Most entries carry no verbs at all**, and that is the ordinary case rather than a gap: there is nothing to expand, and no other per-entry mechanism has been earned yet. `⏎` on an empty menu falls through to a plain activation, which for a transcript entry does nothing. Building the affordance before knowing what it should do is what produced the truncation default this section used to describe.

`←`/`→` (and `C-b`/`C-f`) choose among the items, clamped at both ends; `⏎`/space or a click fires the selected one — clicks resolve against per-frame document-coordinate rects (`block_menu_hits`) *ahead of* the whole-header focus action underneath, or every menu click would just move focus. The selection resets to the leading verb whenever focus moves to another unit. A running tool's peek has no durable `key`, so it carries no menu — the toggle would not survive the next re-projection. A gated call has no result entry at all until it resolves, so nothing there carries a raw flip; its verbs are the gate's, on the strip. Later entry verbs (full-screen, fold, open-in-editor, actions on a backgrounded process's entry) attach here as their mechanisms land.


## Inline approval gates

The approval gate is ephemeral control state: the daemon gates *after* appending the `tool_use` to the log and *before* running the tool. The transition arrives as a `gate` state frame — the topic's whole value when a gate registers, `null` when it resolves — decoded into a `TuiEvent::Gate`. Delivery is coherent with the log ([control-api](control-api.md)): the frame never precedes the entries it reflects, so the gate renders against a transcript that already shows its call.

The gate renders **inline at the pending call**, not as a bottom banner. Because the loop appends the assistant `tool_use` before it gates and one tool call runs per turn, the pending call is always the tail call with no result. `project` receives `pending_call` + `pending_reason` and emits the gate beside that call, where the result will land — the spotter's reason (when it gated via the spotter — read from the `Spot` audit entry, see [permissions](permissions.md)), then the affordance carrying the `⏸`:

```
❯ bash · rm -rf /tmp/scratch
spotter gated · deletes outside the git tree — not reversible
⏸ awaiting your approval  ·  ⏎ approve · type to redirect
```

Reading order is call → what the spotter made of it → what to do, so the affordance lands adjacent to the strip carrying the verbs.

**Every judged call carries its verdict, not only a held one.** The `Spot` entries are indexed by `call_id` alongside the results and emitted with the call they judge, so a *cleared* call shows its reason too — which is most of them, since clearing is the common verdict and nothing ever interrupts for one. The stance leads the line (`spotter cleared · …`, `watching`, `gated`) because it is the scannable part; without it every verdict looks alike. The whole line sits in the dim register: before the decision it is why the call is held, and after it is a gloss on what the call was taken to be, so it never competes with the call or its result for attention. A resolved gate keeps its verdict, which is the only surviving record when a human approves something the spotter wanted to stop. `Origin` is not drawn — a fail-closed verdict's own reason reads "spotter unavailable", so a glyph would only repeat the line. The committed entry is the source; `pending_reason` from the gate covers only a client holding the gate without the `Spot` entry behind it — a window the stream's coherence rule closes (a `gate` frame never precedes the entry it quotes, [control-api](control-api.md)), so the fallback survives as robustness rather than as a path that renders (it has no stance, and reads `spotter · …`).

Whether this much per-call annotation earns its space is an open question the dogfooding loop is meant to answer; a suppression setting is the obvious lever if it turns out to be noise.

**What the harness says about a call is its own entry, never inside the call.** An `Annotation` entry is a different rung's judgment *about* the call rather than part of the call's content, so it sits beside it — and, not being agent-generated, it is not a focus stop. It is read in place, not visited. The reading order call → verdict → affordance/result comes from entry order alone, and the no-blank-line rule (`starts_group`) is what makes the run read as one unit.

The split was originally forced: while the reason and the affordance were nested inside the exchange body, a multi-line command filled the collapsed body on its own and clipped both, and the `⏸` was replaced by the disclosure triangle the renderer substituted once a body was collapsible — so a gate on any realistic command showed no reason, no affordance, and no sign the call was held. That truncation is gone, but the split is kept on its own merits: the `⏸` belongs to the gate, which is something the harness did, not to the call.

The status bar mirrors this affordance for the scrolled-away case, and the [control strip](#the-control-strip) simultaneously carries the full verb set (`approve ⏎ · allow… · abort`), so `↑ ⏎` approves without typing and the strip — not instruction text — is the legend for the richer verbs. Strip `approve`/`abort` resolve the gate directly *without* clearing a drafted redirect the operator decided against sending (unlike an empty-composer `⏎`, which routes through the submit path below).

Submitting from the composer while a gate is pending resolves it with the same three-valued `Approval` the loop consumes (from `src/frontend.rs`):

- empty → `Approval::Approve`
- `.abort` → `Approval::Abort`
- `.allow [prefix]` → `Approval::ConfigEdit` — add a rule, then run the call iff it now clears (the mechanism re-checks; nothing rides along unchecked, see [permissions](permissions.md)). An explicit prefix grants that bash command-prefix; bare `.allow` derives the grant from the gated call (the bash program, or a structured tool's name). Written to the session's `config.toml`, like every committed grant.
- any other text → `Approval::Redirect(text)`

The `.abort` / `.allow` sentinels are retained as the submit *mechanism* — the strip's `allow…` stager commits through the same `.allow` path, so grant semantics live in one place — they're just no longer advertised as things to type.

Resolving replaces the inline affordance with the real result entry (or, on redirect/abort, an error result), and the call stays in the transcript with that outcome in place of output.

## The sidebar

When the terminal is wide enough (≥ a threshold), a **right-docked sidebar** carries the glanceable, self-contained widgets that don't belong in the transcript: `PROCESSES` (the live background processes, off the old bottom strip), `STATS` (context-window occupancy — the `ctx 7% · 14k/200k` line, from `/state`'s `usage.context` — billed session cost where the provider accounts per request — `cost $0.0421`, from `usage.cost`, see [transport](transport.md) — triage retention — `kept 55% · 13k/24k lines`, from `usage.retention`, see [redaction-fork](redaction-fork.md) — plus token/cache telemetry, off the status bar), and `POLICY`. It is chrome, fed by the `usage`/`processes`/`role` state topics (`tui/render_sidebar.rs`), with a `│` dividing it from the transcript; the composer spans the transcript column only, and below the dock threshold the sidebar is hidden (a narrow-mode drawer is future work). Its `Rect` is remembered so `Alt-S` can screenshot just those columns.

`POLICY` renders the **review level** (`/state`'s `review_level` — who reviews a gated call, read-only here; set it with `efference review-level`, the interactive gear selector deferred to the ergonomics pass) followed by the effective perimeter — scope, commands, paths, tools — read from `/state`'s `role`. Pending **proposals** (from `/state`'s `proposals`: prefix-promotion, a session's config-write) render *in the same section, in their committed sort position*, flagged with a trailing `⊕` in a muted style — a proposal is a projection of its committed self, so committing or cancelling it drops the `⊕` in place without reflow — from the focused row's verbs (below), or over `PATCH /proposals/:seq` by any other client. This is the human/orchestrator surface for the config-edit loop (see [permissions](permissions.md)).

**Sidebar focus** (`tui/sidebar.rs`). `Tab` in command mode toggles the column into the focus model: `↑`/`↓` walk the *actionable* rows — processes first, then proposals in their committed sort position; section headers, `STATS`, and committed grants are readout and skipped — clamped at both ends, since the column is not an axis stop. `←`/`→` choose among the row's verbs (`kill` on a process; `commit`, `cancel` on a proposal) and `⏎` fires the selected one; Tab again returns focus to where it entered, Esc homes to the composer. A focused row carries the transcript unit marker **`▌`** in the pad column beside the divider and its verb menu trailing the row's own line — the block menu's grammar and reserved-marker-cell rule, one layer over — so focusing never reflows the column. Rows are held by **identity**, never screen position: a process by its id, a proposal by the `seq` of its `Proposed` entry, and every activation re-resolves against current data. A row that vanished between focus landing and `⏎` — process exited, proposal resolved by another client — no-ops with a status note and leaves the column open-but-unselected rather than firing whatever occupies the position now; the next arrow key re-selects from an end. Verbs dispatch over their own channel (`tui/run.rs`) to the tokio side, which issues `DELETE /processes/:id` or `PATCH /proposals/:seq`; clicks resolve against per-frame recorded rects like the strip and block menu.


## Iterating the display without being blind

`tui/harness.rs`'s `Screenplay` (test-only) scripts a transcript through the *real* event and input handlers, renders to a ratatui `TestBackend`, and frames the buffer as a ruled character grid pinned by an `insta` snapshot — so a wrap or layout change is a legible diff. The grid drops color by design so that `cell(x, y)` / `find(needle)` assertions can check foreground styles and modifiers (diff green/red, error red, the user's blue accent gutter) that a snapshot alone cannot show. Keys route through the full `handle_key_event` (`Screenplay::route_key`), so focus transitions — the spill, Esc-home, unit navigation — are exercised as production code, not a synthetic shortcut.

Two facilities close the gap between *what a snapshot shows* and *what is actually reachable*. The grid encoder is one shared module (`tui/grid.rs`: `frame_grid`, and `frame_grid_region` for a sub-rectangle); a **dev screenshot keybind** (gated by `EFFERENCE_DEV`) runs it against the *live* `InteractiveState` and writes to `.efference/screenshots/` — `Ctrl-S` captures the full screen, `Alt-S` just the sidebar columns (a narrow capture of a wide terminal) — so a live capture is byte-comparable to a committed `.snap`. And `Screenplay::replay` feeds a real recorded log (`persist::load` of an `.efference/*.jsonl`) through the same handler, so a snapshot taken at index *n* is a state the daemon actually produced — reachable by construction, not hand-authored. A trimmed real session lives in `tui/fixtures/`.

The two oracles answer different questions and neither substitutes for the other: the character grid is the **layout** oracle (wrapping, columns, focus geometry — a legible diff), and **pixels** are the appearance oracle. Appearance review happens outside the repo: drive the real TUI (`efference gallery`) through a `vhs` tape to a PNG. A harness-only state that `gallery` can't reach has no pixel path yet — that is what a `Buffer → ANSI` encoder beside `frame_grid` would buy (see [`../backlog/tui-polish.md`](../backlog/tui-polish.md)).


## Lifetimes

The IR keeps ferritin's `Cow<'a>` borrowing, used as `Document<'static>` over owned strings. Nodes are replaced wholesale rather than edited in place — the incremental projection splices a rebuilt entry's nodes over its old ones — so the borrowing costs nothing here, and staying byte-identical to ferritin keeps improvements backportable both ways.


## Backlog

Remaining polish — the box→`│` connector refinement for result bodies, per-process affordances beyond kill (bring-to-foreground has no daemon transition; jump-to-entry), the narrow-mode scrimmed drawer, the at-rest unit boundary, and refreshing the help-screen keymap text — is tracked in the backlog.
