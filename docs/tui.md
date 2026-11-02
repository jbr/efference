# The TUI client

The TUI — what a bare `efference` draws ([operating.md](operating.md)) — is the human-facing client. It is just-another-socket-client: it subscribes to the daemon's SSE streams over the same [control channel](control-channel.md) the `control` tools use, and it drives turns and approvals through the same HTTP verbs. The renderer is vendored-and-adapted from ferritin's interactive renderer into `src/clients/tui/`. Ferritin already had the hard parts — a general-purpose document IR, a markdown→IR parser, syntect theming, a custom scrollbar, and line wrapping — and its render pipeline was domain-free (it renders an IR, not rustdoc), so the lift was strip-the-shell, not rewrite. Improvements flow back cleanly because the IR stays byte-identical to ferritin.

**Display settings** are the `[tui]` table of the user configuration
(`~/.efference/config.toml`, `src/config/user_config/tui.rs`), not flags: `theme` (a built-in
name or a `.tmTheme` path, `~/` expanded; "Catppuccin Frappe" when omitted), `images`
(below), and `frame-cost` (below). A TUI is drawn by more than one surface — the picker's
attach, `--name`, a terminal island, setup's offer — and each reads the same table once at
launch; a switch or a hand-off to another session keeps what was read. The host never
reads it. `dev view` and `dev gallery` take
`--theme` and `--images` (and `gallery` `--frame-cost`), each overriding its key for one run.

`efference dev gallery` (`tui/gallery.rs`) launches the real TUI on a fixed "one of each element" transcript with no daemon — a live design page that surfaces rendering gaps (literal `~~strike~~`, ordered-lists-as-bullets) the moment you look.

`efference dev view <session>` opens a **recorded** session the same way: the entries come from a session's `log.jsonl` (via `persist::load`, the same decode a live session's entries arrive as) rather than from a daemon's event streams. The session is named — a session id under `.efference/`, a session directory, or a path to a log — and an unresolvable name lists the recent ones rather than opening an empty view. Both offline entry points share one runner (`tui/run.rs`'s `run_offline`), which seeds the event channel and holds every channel far-end for the session's life, so the UI never sees a closed channel where a daemon would be.


## Read-only: what a view drops

`efference dev view` is inert by construction, not by disabled buttons. `InteractiveState::viewing` holds the session's label and is what `read_only()` reports; four things follow from it, each because the thing it removes only exists relative to a running daemon:

- **No composer.** It reserves no rows (`render_frame`), so the transcript takes them. An input that cannot send is not chrome worth drawing.
- **No loop verbs on the [control strip](#the-control-strip).** The whole strip is the client-only tail — `theme t · help ? · close q`, with `detach` reading `close` because there is no daemon left running behind it. Pace goes with the rest: it is a control over a loop, and there isn't one.
- **No session drawer.** Every section of it (processes, usage, policy) is fed by the `state` frames of a live control stream, and there is no daemon to serve one.
- **The strip is the resting focus,** in place of the composer: focus starts there, `Esc` returns there, and `↓` off its bottom stops rather than falling into a composer that isn't rendered.

The log is read once at open, so a session that is still running shows as of the moment it was opened — a snapshot, not a tail. Reading a live session's log is allowed for that reason rather than refused; nothing in the view can write to it.


## The transcript is another `project(log)`

Just as the wire is a deterministic projection of the log into the request its dialect requires (see [log-and-projection.md](log-and-projection.md)), `tui/projection.rs`'s

```rust
project(&[LogEntry], &LiveTail, &ViewState, Gate<'_>, pending_turns: &[SubmittedTurn]) -> Document<'static>
```

is a *second* projection of the same log to a different target: a renderable tree instead of a model-facing message list. It reuses the wire projection's rules where they apply — selection-else-raw for tool results, the streamed-vs-committed dedup (where the wire uses `streamed_*` flags, the TUI projection uses the structural `LiveTail`) — and diverges where a human reader differs from the model: nothing is elided or coalesced (the human sees the whole record), and the projection carries a deliberate **information architecture** — a typed entry per wire block, decorated by what it is, rather than a dump of labeled blocks.


## Information architecture: a flat sequence of entries

<a id="line-structured-text"></a>**Line-structured text vs. blocks.** An `Entry` separates its child *blocks* by a blank row, which is markdown block spacing and right for prose. Text that is already line-structured — streamed tool output, an arriving tool input, a multi-line pending turn — is not a sequence of blocks, and modelling one source line as one `Paragraph` doubled every newline the source actually had. `DocumentNode::Lines` is a run of rows with no separator, each wrapping like a paragraph; the live renderers use it. Thinking is markdown, and goes through the markdown renderer inside a `Dimmed` wrapper (below).

The projection emits a **flat** sequence of `Entry { kind, marker, header, blocks, key }` — one per top-level thing on the wire, decorated for presentation. There is no conversational-turn container: a turn's thinking, its prose, and each call it made are separate entries, in wire order.

`EntryKind` is the taxonomy an operator actually scans by — **what I said**, **what the agent said**, **what the agent did** — rather than the wire's roles:

| kind | is | rendered |
|---|---|---|
| `UserMessage` | what I said | accent gutter down the left margin |
| `AssistantMessage` | what the agent said | the unmarked default voice |
| `Thinking` | what the agent thought | `✻ thinking`, dimmed register |
| `ToolCall` | what the agent did | its intent as a title, then `› tool(arg, key: …)` |
| `ToolResult` | what came back | `result (…)` naming the wire state, `✗` on failure, over the output |
| `RunningTool` | what is coming back | `⋯ running · N lines`, over the peek |
| `Annotation` | what the *harness* says about a call | dim line, no marker |

A tool result occupies a `role: user` block on the wire, but it is the harness speaking rather than the operator, so it is its own kind and never carries the gutter. Authorship is **structural, not lexical**: you *see* who is speaking instead of reading a `▌ you` / `▌ assistant` noun.

A call and its result are **separate entries**, because they are separate blocks on the wire and a result is not part of a call's content. They read as one unit anyway: a `ToolResult` and an `Annotation` do not open a new visual group (`EntryKind::starts_group`), so no blank line separates a call from what the spotter made of it or from what came back.

```
Say hello.
› echo hi
spotter cleared · reads only, inside the tree
result (raw)
line one
line two
```

The result has no `#N` / `[seq]` label — it is obviously the response to the call directly above it. Its header row is worded as the web client's result head is: `result`, then in parentheses the wire state and what was kept — `result (raw)` (a natural raw result, or the operator's flip), `result (triaged, kept 3 of 412 lines)`, a bare `result` when triage kept every line (the view is everything the model saw), plus `1 image`, the control channel's `16 KiB of 256 KiB shown` cut, and `error`. A failed result also carries the red `✗` marker (on the entry that actually failed, rather than on the call, which is a call regardless of how it went); one that went fine carries none. The header is also the focus target the raw/triaged verb hangs off.

**A call reads as its intent, then a pseudocode signature** — the intent as the entry's title row (what it was for, before what it was), then `› read("src/log.rs", from_line: 3)`, `› check_inbox()`, `› write("src/main.rs", overwrite: true)`, or for `bash` the command itself after the prompt (`› cargo test`, tagged `background after 600s` when it asked to be). A call without an intent starts at its signature. The form is stated once, canonically, in [web-client.md](web-client.md) ("A call reads as a signature"); `projection/signature.rs` is a port of the web's `signature.ts` and `signatureLayout.ts`, with their test cases, so the two clients agree: a built-in's primary argument reads positionally (an array spread), a multi-line value is content and renders below the signature captioned by its key (`Captioned`, the caption directly on its block), and the edit family emits no bodies because its diff *is* the representation.

The signature is a `DocumentNode::Signature` laid out at **render** time, where the width is known: a call that fits is one line; one that does not breaks after `(` with an argument per row, and a collection that still does not fit breaks inside the same way; a lone scalar argument stays hugged by its parentheses; a single value longer than three lines at that width leaves the signature and renders beneath it under its key. A value that runs past the width wraps hung under its own row's start. In terminal terms: the `<server>` segment is a dimmed span ahead of the dot (the `open_world_hint` distinction), the tool name is `FunctionName`, keys are `FieldName`, values and a `bash` command are `InlineCode`, the prompt is the accent colour.

The live block renders through the same `call_view` from the settled half of a partly-arrived input, so a call fills in rather than changing shape when it commits; the intent fills in as the title while it streams, and `arriving` puts the `…` *inside* the parentheses, so a signature never reads as closed before it is (the web appends it after the `)`).

One divergence from the web form, forced by position rather than chosen: **call bodies are not truncated** — the web budgets them at 12 lines, and the TUI's budget for every body is the whole of it (below). Arguments read in **sorted key order** on both clients alike: a committed call's input is a `serde_json::Value` whose object keys sort (the wire's byte stability depends on it — [log-and-projection.md](log-and-projection.md)), and the web parses the same sorted log text.

Results get **per-tool representations** instead of a raw text dump:

- `bash` — rendered as a shell code block; a single-line command goes bare on the call header, a multi-line command overflows into the call entry's body.
- `read` — strips the sentinel framing (`==<token> BEGIN <path>, BYTES: n` … `END`; older logs say `LENGTH:`) and highlights the file body by extension. The path is everything between ` BEGIN ` and `, BYTES:` (or `, LENGTH:`), so fields fs-mcp appends after the size (a windowed read states `, LINES: a-b`) do not move it. A window that starts past line 1 also prefixes each body line with its file line number and a tab; the TUI shows those prefixes inline.
- `edit` / `undo_edit` / `retarget_edit` — the result's `===DIFF===` section parses into a color-coded `DocumentNode::Diff` (added/removed/hunk/context lines). The edit *input* is never dumped: the diff is the representation of what happened, so the call signs its arguments and emits no body.
- Anything else — a plain code block of the content.

A **triaged** result is a case of its own. The wire renders a verdict as `@@ lines a-b @@` headers over the kept lines, because text is all it has to say "these came from there" with (see [the triage](triage.md)); a terminal has a gutter, so the transcript renders the same lines in a `DocumentNode::NumberedCode` — each carrying its original line number, dimmed, with the gaps stated by the jump in the numbers instead of by a header row. Both views resolve the verdict through the one `project::selection`, so they can differ in form but never in *which lines* they show — an operator comparing their view against the agent's is the whole point of the view.

**A tool with its own representation keeps it, over the triaged lines** (`triaged_body`). The tool-specific parse runs over the *whole* result and produces rows that each carry the raw content line they came from; the kept-line filter is then applied to those rows. So a triaged `read` is still framing-stripped and highlighted by extension, and a triaged edit's surviving lines are still classified as a colored diff — at whatever subset triage left. The one line mapping is what makes both possible at once: parse first (which needs the whole output), select second (which is expressed in raw line numbers). A `read`'s sentinel lines carry no row, so a verdict that kept one simply shows nothing for it; the numbers in the gutter are the *result's* line numbers, not the file's, because that is the coordinate system the verdict was expressed in.

The result header states the divergence in one place: `kept 3 of 40 lines`. A `Diff` node has no gutter, so a partial diff cannot state its own gaps — the header count is what says the view is partial there. When triage kept every line there is no divergence to state, so the header says nothing either: `kept n of n` names only that triage ran, and the head presents like any whole result.

The default view being the triaged one is a **reversal**, taken from live use: a large `read` triaged down to a few lines was rendering in full, and the difference between what was on screen and what the agent was working from was the thing the surface exists to show. The earlier position — that a subset of a diff is a *mangled* diff, not a smaller one, so rich-rendering tools should show the whole output and annotate the gap — is what the row mapping replaces: the rendering no longer has to be traded against the selection.

The triager's **note** is not part of the result. It is that rung's account *of* the output, the way a spotter verdict is an account of a call, so it renders as its own `Annotation` entry beside the result rather than as a trailing line inside its box — where it read as something the tool had printed. Triage's request for a better `intent` (`intent_feedback`, [triage.md](triage.md)) is a second such annotation, shown by default, and the breadcrumb for lines the peek cap dropped before triage a third. All disappear when the result is flipped to raw: raw is what the tool actually returned.

A code block is highlighted, and carries its `❬lang❭` corner tag, **only when the language is known**. An untagged block is plain text in a bare border. Ferritin's renderer defaults to `rust`, which is right there (a rustdoc fence is Rust unless it says otherwise) and wrong here, where the commonest untagged block is a tool result: `ls` output labelled `❬rust❭` and coloured as Rust.

Content **wraps to the box's inner width** (`render_code_block`), through the same `wrap_points` break policy prose and the composer use — one break oracle, not a second wrap path. Wrapping is **linear in a line's length**: every width comparison in the policy is against something no wider than a row, so widths are measured only that far (`width_upto`) and never to the end of the text. Measuring the whole remainder once per row emitted is quadratic: a 16k-column line (a tool call's raw JSON input, a minified file, a base64 blob) costs ~300 ms per wrap in release, on every frame while it streams. A line longer than the frame would otherwise run past the right border and be clipped there, which makes untruncated output worth much less than it looks: a stack trace or compiler error is exactly the shape that overflows. Row boundaries partition a line with no gaps, so each highlighter token is clipped to the row it appears on and nothing is dropped between rows.

The throughline: lean on pre-attentive cues (color, gutter, glyph) and tool-specific shape, not repeated labels.


## Detail by recency

**How much of an item renders depends on its distance from the tail**, as in the web client ([web-client.md](web-client.md), "Detail by recency"); the rules are `tui/detail.rs`, the rendering `tui/render_detail.rs`. Each top-level node of the transcript is an item — a user turn, a run of prose, a thinking block, a call with everything that reads as part of it (the spotter's verdict, the gate affordance, the result, the triage note), which the projection groups as one `CallItem`. The last 6 items render whole; items 6–20 back are **trimmed**; older ones are **brief**. User turns and prose always render whole.

- **A call, trimmed**, renders whole except its result body, which is cut at 6 rows: the last row shown is dimmed — the terminal's stand-in for the web's fade — and a `⋯ show the whole result` row under it opens the item. **Brief**, it is its intent, its signature on one row after the prompt (cut with `…`), and the triage note.
- **A call that must show** renders whole at any distance: no result yet (running, or held at the gate), a failed result, output withheld by a failed triage, or a spotter verdict that gated or watched it.
- **A thinking block** of 120 words or fewer renders whole at any distance — folding it saves nothing. A longer one at the whole level is held to its last 8 rows under a `✻ thinking · N words · show all` header (the box it streamed in, stopped at its last line); trimmed, its first 3 rows and `⋯ show all N words`; brief, one `✻ Thought · N words` row. A thinking item is keyed by the seq of the assistant entry it came from.

Every folded form opens whole on a click, or `⏎` with it focused — a folded item is one stop on the `↑`/`↓` axis, through its `ExpandItem` action. What is open is render state (`InteractiveState::expanded`), not view state the projection reads: a level is a render-time choice, so the projection stays a function of the log and an item's level changes as the tail grows without re-projecting anything. The document walk computes each node's level and stores it with the node's cached layout, so a node that drifts to a new level is laid out again; opening an item invalidates the layout cache. A trimmed body or a held thinking box is laid out in full and drawn through a row clip (`LayoutState::clip`, honoured by every write path, images included), so the folded form is the same nodes a whole item renders, cut, rather than a second rendering of the content.

What this replaced was rendering every entry whole, a starting position ([positions.md](positions.md)) held until reading real sessions said what was worth folding: a long session became thousands of rows of results and thinking the operator had already read, with the recent work at the bottom of it. The earlier generic fold — a shared 8-line budget over a body built as *the call's argument overflow followed by the result*, so a long `bash` command consumed the budget and hid the result — is the defect this keeps in view: what folds is old, and what is recent, failed, or flagged stays whole.

**A summary** (docs/summary.md) is drawn as the web client draws it: a rule, then `⊙ consolidated #from–#upto · N entries` (`through #n` for a head summary, `compacted` for a compaction) — the extent, counting the turns, completions and results in the range rather than tokens, which are provider accounting the client does not hold — then the summary text that replaced the range, rendered as the markdown it is, then a closing rule. It is drawn after the entry its range ends in, where the agent's projection puts the text, not at the summary's own log position: `CallIndex` records which entry draws each seq (a result is drawn by the entry that issued its call), and a summary's arrival dirties that entry in the incremental transcript. Every entry it replaced is still on screen above it, so there is nothing to expand. The resolved `consolidate` call above the mark collapses to its bare name, `› consolidate`: its input is the summary the mark renders, and its result is a constant acknowledgement. A pending, gated, or failed one renders whole.

**Hover and click.** A call item or a thinking item tints under the pointer: every background-coloured cell across its rows takes a surface one step toward the text colour (`theme::mix`), so the call, what the spotter said, and what came back read as one card, as the web client's hovered call does. A click on any row of one focuses it, not only on its first row. The walk records each item's rows (`RenderCache::items`) for both. A control-strip item under the pointer lifts the same way (focus keeps its own surface; a greyed slot does not lift), and the header's button takes the strip's focused style. Over any of these — a link, a strip item, the header button, an item — the terminal pointer becomes a hand where the terminal honours OSC 22 shapes (`utils::set_cursor_shape`).

Two consequences worth stating, since both look like omissions otherwise:

- **A focused entry usually has no verb.** Focus is an affordance in itself; `⏎` on an entry with an empty [block menu](#the-block-menu) does nothing, rather than reaching for a verb it doesn't have. Focusable-with-no-action is the ordinary state, not a gap.
- **A result the control channel truncated is named, not silently shortened.** An oversized tool result arrives on the stream as a prefix carrying `content_elided` ([control-api.md](control-api.md)); its header then reads `… · 16 KiB of 256 KiB shown`, sourced from the tag rather than from measuring the string, which is only ever the prefix that arrived. The TUI does not fetch entries by `seq` today, so the marker says what is on screen — not that it can be completed here.
- **A running tool's peek is not a fold.** A *running* tool's output is unbounded and untriaged, so the live tail shows a **peek** — the last `PEEK_LINES` (10) lines, dimmed, under a `⋯ running · N lines, last 10` header. It is a glance at a live process, not a way to read its output; the whole output arrives, whole, when the call commits. The peek is the *default*, not a cap: the entry's [block menu](#the-block-menu) carries `full`, which shows everything that has arrived, and the choice is held against the call's `call_id` so it survives the fragments that keep arriving under it. A full-screen pager for a live process is still future work.


## Result images

A tool result's images render in the transcript through the terminal's graphics protocol (`tui/image.rs`, `tui/render_image.rs`), via `ratatui-image`. The pieces and the invariants they hold:

**The node is a reference; the store holds the state.** `DocumentNode::Image` carries only the log address (`seq`, `idx` — the coordinates `GET /log/:seq/images/:idx` dereferences), the media type, and the pixel dimensions from the entry view's metadata. Everything stateful — fetch lifecycle, decoded pixels, per-protocol encodings — lives in an `ImageStore` on `InteractiveState`, keyed by that address. The split keeps the projection pure of the terminal's capability, and keeps `DocumentNode`'s derives (a `Protocol` implements neither `Debug` nor `PartialEq`).

**The reserved rectangle is a pure function of metadata and render width** (`ImageStore::footprint`) — never of whether bytes have arrived, decoded, or failed — so byte arrival repaints the reserved rows and reflows nothing. A test pins the document height as identical between the in-flight and landed states. The natural size (pixels over the queried font size) rounds *down*: a ceil'd final row is mostly aspect-fit padding, which iTerm2 composites as an opaque band. The footprint caps at 24 rows — sizing, not truncation; the fit scales, nothing is cut. Residual sub-cell padding is encoded in the theme's background color (`set_theme_background`, re-applied on theme change with the encodings dropped), so it disappears into the transcript under protocols that render the default transparent pad opaquely.

**Two byte sources, one render path.** A live entry view omits `data` (docs/control-api.md), so `handle_event` marks the slot fetching and hands the address to a tokio-side fetcher (`Client::log_image` over the socket), which decodes off the UI thread and answers with a `TuiEvent::Image`; a failure lands too, and the reserved rows carry the reason. An offline log (`efference dev view`, the gallery) delivers the base64 inline, decoded at the event fold. Slot presence gates both, so a reattach replay resolves nothing twice.

**Encoding is exactly-once per (image, footprint)**: the detected protocol's escape for the fully-visible paint, plus a halfblocks scratch buffer — ordinary cells, rendered once through the real widget — for everything the escape can't do. A resize re-encodes at the new fit; nothing re-encodes per frame.

**The paint rule** (`render_image_node`): fully visible → the native protocol's escape (the widget's cell-skip keeps ratatui from overwriting it); partially visible, top *or* bottom → a row-slice of the halfblocks scratch, blitted cell-by-cell — the pixel protocols paint only from the image's top edge, and iTerm2 clips not at all, so cells are the one representation any sub-range of rows has. While the viewport is *moving* (`layout.scroll_in_motion`, set by `render_frame` comparing the settled offset against the last painted one), a protocol with no stored-image concept (iTerm2's OSC 1337, sixel — every reposition retransmits and re-decodes the payload, which flashes) holds the halfblocks slice; a follow-up settled repaint, scheduled by leaving `dirty` set, restores the crisp render. Kitty stores and re-places, so it stays native through motion.

**Detection is a startup act.** `Picker::from_query_stdio()` runs in `ui_loop` after raw mode and before the crossterm reader thread takes stdin. `images = "auto"|"kitty"|"iterm2"|"off"` under `[tui]` overrides it in both directions, because tmux makes detection lie both ways. On auto, a terminal whose environment identifies iTerm2 (`TERM_PROGRAM`/`LC_TERMINAL`) always gets OSC 1337: iTerm2 answers the capability queries above its own weight class — DA1 advertises sixel, 3.5+ answers the kitty query partially — and renders the query-ranked winners as garbage (backlog/inbox/tui-image-sixel.md). With images off (or no capability), an image renders as its one-line reference, no reservation. The resolved protocol and font size print on the status line at launch.

**Snapshots pin halfblocks** (`Picker::halfblocks()`, the deterministic constructor — `Screenplay::halfblock_images`): the mosaic is ordinary cells, so the fiddliest part — the partial-scroll arithmetic — is grid-snapshot-testable, while the protocol modes stuff escape blobs into cell symbols and stay out of snapshots. The gallery carries a generated test card (hue sweep for color depth, a circle that turns elliptical under a wrong font-size answer) with its bytes inline, exercising decode → encode → paint under whatever protocol the real terminal answers.


## View state: surviving re-projection

The transcript re-projects on every streamed token, and any entry can be re-projected at any time (a result or verdict arriving rebuilds the entry that made the call). That means a raw-vs-wire toggle cannot live inside the rebuilt `Document` — it would reset mid-stream. It lives instead in `tui::state` as a `projection::ViewState` keyed by `call_id`, and the projection reads that state when building each result. `ViewState` also records which `call_id`s were toggled, since a view flip is not a log event and the incremental projection has no other way to learn of it.

```rust
struct ViewState {
    raw: HashSet<String>,       // call_ids flipped to raw output
    expanded: HashSet<String>,  // call_ids whose running output is shown whole
}
```

Two toggles, one per fork that exists. **Raw** flips a focused tool result between its triaged wire form and its full raw output; both are already client-side from the log entry's `WireState` and `RawResult` (see [the triage](triage.md)), so this is a pure render choice with no round-trip. When showing raw over a selection, the result header gains a `· raw` annotation so the human knows they are off the default view. **Expanded** is the peek/full fork on a *running* tool's output, the one place the transcript does not render whole (below); it retires when the result commits, since the committed entry renders whole and there is nothing left to choose between.

The generic expand/collapse went with the truncation, and neither of these two is it: each is a choice between two real forms of a thing, not a choice about how much of one to show.

**A live block carries the same key.** Both toggles key on `call_id` because it is the only identity that survives a rebuild — and a *live* block has one from `tool_call_start` (docs/control-api.md), which is what its result will key on too. It did not use to: the running-output peek was built unkeyed, so nothing could be recorded against it, and a block the operator opened to read snapped shut on the next arriving fragment. A live entry with no `call_id` still carries no verbs, for the same reason it always has: the toggle would not survive the projection.


## The live tail

In-flight streaming deltas not yet committed to the log live in a `LiveTail`:

```rust
struct LiveTail {
    thinking: String,             // extended thinking streamed so far this turn
    assistant: String,            // in-flight prose this turn
    tools: Vec<(String, String)>, // (call_id, accumulated output)
    calls: Vec<LiveCall>,         // { index, call_id, name, input }, in arrival order
}
```

**Thinking accumulates, it does not flash past.** Streamed thinking is live-tail *content*, beside the assistant prose buffer — not a single-slot preview. It was one once, and a single slot is overwritten, so `tool_call_start` overwrote it: a turn that thought briefly and then called a tool showed its reasoning for a few frames and lost it until the turn committed, which is most turns. It now stays put while the model moves on to prose or a call, and the committed block supersedes it.

Live and committed thinking are built by **one renderer** (`nodes::thinking_block`), rendered as a `Thinking` entry ahead of the turn's prose in the dimmed register — thinking is never the answer and must not read as it. A test asserts the two produce byte-identical transcript rows for the same text: two renderers would drift, and the drift surfaces as the transcript visibly rewriting itself at the moment of commit, which reads as a bug even when both renders are correct on their own.

**Thinking is markdown**, through the same `MarkdownRenderer` assistant prose uses. Nothing requires it to be, any more than prose is required to be; it simply is — 73.5% of 653 blocks across 20 sessions carry an explicit markdown construct ([web-client.md](web-client.md) holds the measurement, which the TUI inherited rather than re-derived). Rendering it as literal lines left fences, inline code, emphasis, and lists as the characters that spell them. This also *resolves* the regression that preformatting was defending against: reflowing `- a\n- b` into `- a - b` stops being possible once the list is a list.

**The dimmed register** is what markdown-rendering thinking required. The web client gets muting from a CSS class that cascades over whatever the renderer produced; the TUI has no cascade, so `DocumentNode::Dimmed` wraps a subtree and `render_node` sets a flag its style resolution consults (`span_style.rs`). The register is a property of *where* a node sits, not of what a span means, which is why it wraps rather than adding a dim variant per `SpanStyle`.

Inside it, **prose dims flat and code keeps its hue**, blended toward the register rather than replaced by it (`CODE_DIM_BLEND`). The tension is real and this is the chosen point on it: dimming a code block the whole way makes it legible as structure and useless as code, which is the visible reason to render thinking as markdown at all; leaving it at full strength weakens the never-mistaken-for-the-answer property exactly where a reader's eye lands. Blending keeps keyword/type/function separation while the block still sits below the answer's brightness. The colors are invisible in the no-color snapshot grid, so this is a **pixel** question — `efference dev gallery` carries a thinking block with a fenced Rust block for that purpose.

It is shown **in full**, and keeps the model's own lines. Reasoning streams past too fast to read at generation speed, so the committed copy is the one a human actually reads, at their own pace — which it can only be if all of it is here. It is also not flowing prose: it holds lists, indentation, and snippets, so reflowing it to paragraphs turned `- a\n- b` into `- a - b`. (It had to, once: one node per line was one *block* per line, and an entry separates blocks by a blank row, so the whole entry double-spaced. `DocumentNode::Lines` renders a run of lines without that — see [Line-structured text](#line-structured-text).) Encrypted (`RedactedThinking`) blocks render a line saying so rather than nothing, so the record never implies the model didn't think when the provider merely withheld the text.

`project` renders the committed log first, then appends the live tail — thinking first, then in-flight assistant prose as its own entry, then one entry per in-flight call, then each running tool as an output peek. When a committed `LogEntry` arrives, `LiveTail::commit` clears the superseded buffer (assistant prose on `Payload::Assistant`, tool output on `Payload::ToolResult`), mirroring the wire projection's `streamed_*` dedup flags, now structural.

**In-flight calls: a turn that produces no prose is still visibly working.** The streaming sink is typed (`api::Streamed` — `Text` / `Thinking` / `ToolStart` / `ToolInput`), so thinking and a tool call's arriving input reach the client as their own `output` frames (`thinking_delta`, `tool_call_start`, `tool_call_input`) rather than not at all. Thinking accumulates (above); each call in progress renders `❯ calling <tool> · <arguments>…` as its input arrives.

**A collection, keyed by `call_id`, not a slot.** Every model-stream delta names the content-block `index` it appends to and `tool_call_start` carries the `call_id` its result will key on ([control-api.md](control-api.md)), so a fragment binds to the call that opened its block. Held one call at a time, a response with two calls shows only the last — `tool_call_start` displaces the first, and the arguments the operator is watching arrive disappear mid-flight. This is the same failure thinking had, one field over, and it has the same answer: the slot was the defect. Thinking, prose, and calls still render as three fixed buckets rather than in wire order; the deltas now carry what an ordered render would need, and whether that ordering is worth having is open.

Four rules:

- **In full, never truncated.** The point is to watch the work as it happens, so everything that has arrived is shown.
- **Parsed as a prefix, and what may be claimed from it is decided positionally.** A tool call's input mid-stream is a *prefix* of a JSON document, and it is read as one (`jiter`, `PartialMode`): a loose parse keeps the trailing incomplete value, a strict parse drops it, and the one key the first has and the second lacks is exactly the value still in flight. Within an object **only the last key can be incomplete** — the bytes for the earlier ones arrived and closed — so the settled pairs may back an identity claim and the in-flight one may not. `{"path":"src/api.rs","contents":"use st` renders `write(path: "src/api.rs", …)` (true — `path` closed) with the contents as arriving content; a half-arrived `"src/api.r` stays out, because `write(path: "src/api.r")` is well-formed, plausible, and false. Before any pair closes there is nothing to claim and the raw bytes are shown, which claim nothing. See `projection/partial.rs`.
- **One renderer with the committed call.** The settled arguments go through `call_view` and `call_blocks`, which a committed call also renders through, so the header *fills in* as bytes land rather than changing shape at commit. The in-flight value never reaches it. A string value with newlines in it renders as a code block in the language of whatever settled path names it, which is why the identifying argument arriving first is what makes this worth doing — and is why the streamed contents must be *decoded* (a raw JSON input is a single line, its newlines `\n` escapes).
- **Prose supersedes the calling lines**, and a committed turn clears them: prose is its own evidence of progress, and the committed entry is the record — which the transcript keeps (below). Prose leads a response's tool calls on the wire, so this drops nothing a turn is still emitting. Thinking is no longer superseded by either.

Without this the window between submitting a turn and the first prose token was blank, and blank is indistinguishable from a hung harness — a risk of interrupting or abandoning a live session on false signal. The daemon's own log has the matching fix: it prints `(generating — turn N)` before blocking on the model, so its last line is no longer a stale `idle`, which reads as *finished*.


## Pending user turns: the delivery indicator

A submitted turn is not lost while a trajectory runs. `submit_composer` **mints the submission id**, pushes `{submission_id, text}` onto an ordered `pending_turns` queue, and reprojects at once; `project` renders each as a **dimmed, `⋯`-marked user turn** after the live tail — the optimistic "handed to the daemon, not yet on the wire" echo. When the `User` `LogEntry` carrying that id streams back via `/events` (that entry *is* `project(log)`, i.e. now on the wire), `handle_event` drops it from the queue and it re-renders as a normal committed turn. Keyed by id, not by content: recall (below) hands a queued turn back to be edited and re-sent, at which point a content match would settle the wrong echo. The pending→committed flip is a faithful delivery signal, not cosmetic: it mirrors, on the operator's side, the daemon's mid-trajectory delivery of a queued turn ([operator participation](control-channel.md#operator-participation--the-intervention-points), [control channel](control-channel.md)).


## The pace indicator

The operator's pace ([operator participation](control-channel.md#operator-participation--the-intervention-points)) arrives on the `pace` state topic and is surfaced **loud** in the status bar: a `⏸ HELD` / `⏱ 10s` indicator ahead of the status text whenever the pace is non-default (the default fast-and-unheld state adds no chrome). It is status-bar chrome, never transcript content — pacing never reaches the log or the wire. The transcript keys `+`/`-` step the speed dial along a coarse ladder and `p` toggles the hold, each optimistically writing the stored pace on the keypress (the topic's next frame replaces it) and issuing `PATCH /loop {pace}` over a dedicated pace channel. Because a slow pace *is* the window during which the operator has room to read and type, pacing and the mid-trajectory injection compose: the sleep is exactly when a message can be interjected.

The `upstream_failure` topic carries an optional upstream-failure diagnostic. A non-2xx response renders as `upstream 429 ×2` ahead of the status text; the count covers consecutive matching statuses. The daemon clears the topic on the next 2xx, so the indicator disappears on recovery rather than persisting as a survived failure. It is never transcript content.

The `upstream_retry` topic carries the retry in flight ([transport.md](transport.md)), rendered ahead of the failure indicator — it is the live state, and it says what is being done about the status the failure indicator names. The daemon's own lead line is shown verbatim after a `↻`, so the status code is never the headline, followed by the gesture that skips the wait: `↻ attempt 2 of 12 failed upstream, retrying · ⏎ retries now`. A triage's retry takes a `triage:` prefix, since that wait sits between tool results rather than in a generation. The topic is level-triggered — `null` is the all-clear and removes the indicator. The status bar renders no countdown: the topic's `next_attempt_at` and the upstream's `detail` text are read past, because the bar does not tick and the error body belongs in the log.

The `current_operations` topic ([control-api.md](control-api.md)) names what the loop is waiting on, rendered as the first status-bar indicator after the pace, while the loop phase is anything but `idle` (an idle triage included): `waiting on <model> · 2m 10s`, `<model> streaming`, `triage · waiting 40s`, `spotter judging · 3s`, `bash: <command> · 45s`, `retrying upstream · attempt 2 (429) · 8s` (`operations.rs`; the wording matches the web turn state). A `bash` call is labelled by its `command` (cut at 48 characters with `…`), other tools by their intent. The `gated` and `asking` phases take the slot instead — `waiting for your approval` / `waiting for your answer` — since the operator is what the loop waits on (`status_operation_text`). The topic carries absolute instants, so the text is derived against the client clock — after each frame and again by `refresh_clocks` each second — and stored in state, which keeps rendering a pure function of it. The transcript draws nothing for the `RequestStage` entries behind it.

The `stall` topic carries the terminal counterpart ([transport.md](transport.md)): a trajectory that gave up rather than finished, rendered as `⚠ <the daemon's lead line>` after the transport indicators — it is a condition that persists at idle, where the two above are live transport state. The daemon's sentence names what happened and is shown verbatim, followed by the recovery (`· ⏎ to retry`); the topic is level-triggered and clears when the work resumes. A triage held on the upstream sets it with the loop `blocked` rather than idle. It is what tells an operator glancing at an idle session that the run stopped short, which the idle phase alone does not.

## Graduated Esc


`Esc` is a **state machine, not a press-count** ([operator participation](control-channel.md#operator-participation--the-intervention-points)): each press's effect is a pure function of the current state — so there is no counter, no timing debounce, and nothing to reset. Precedence, gentlest first (`handle_escape` in `tui/state.rs`):

0. **an inline sub-state is open** (the pace [widget](#the-control-strip)) → revert it, restoring the value at entry. These previews were never committed, so this only undoes local state.
1. **not at rest in the composer** (an overlay is open, or focus is in the transcript) → return to the composer (`Esc` is one-directional toward the composer).
2. **at the composer, held** → **interrupt** the in-flight tool (`POST /loop/interrupt` — the loop stops at the next juncture and the running call's output is withheld from the model, plus, on a foreground bash, its process group is killed; see [control-channel.md](control-channel.md). Mid-tool is the one point pacing can't reach).
3. **at the composer, not held, loop active** → **pause** (set the `held` overlay via `PATCH /loop {pace:{held:true}}`; the loop stops at the next between-step juncture).
4. **at the composer, not held, nothing happening** → **swallow** (a silent no-op).

So sitting in the composer while the agent works, *one* `Esc` pauses and a second interrupts — a running tool is killed, a generation in flight is ended ([control-channel.md](control-channel.md#the-interrupt)) — no wasted "return to composer" press; and jamming `Esc` from anywhere still walks return → pause → interrupt. "Loop active" is read from `/state`'s **`loop_phase`** — any phase but `idle` — unioned with the streaming `pending_request`; the phase, unlike streaming deltas, stays `executing` through a *silent* long-running tool, so `Esc` can still reach a runaway command. `Esc` only ever *enters* the hold or *fires* the interrupt; resume is the pace layer's job (send a message — submit always resumes — or the `p` key). The hold and interrupt verbs are exactly unit 2's `/pace` and the long-standing `/interrupt`, so this is a TUI-only key binding over existing daemon affordances.

Alongside kill, transcript-navigation `b` fires the **foreground→background handoff** (`PATCH /processes/:id {background: true}`, [process-monitor.md](process-monitor.md)): the in-flight tool keeps running as a `p#<n>` process instead of being killed or waited out. When the loop is idle the key is a status-line no-op (nothing sent). There is no separate note-capture flow — a message typed after `b` is delivered to the model mid-task alongside the handoff result. Both intents share one channel to the tokio side (`tools::Interrupt`: `Kill` → the loop interrupt, `Detach` → the process patch, whose foreground-id resolution the client verb owns).

> **Discoverability.** Pause is reachable where the operator lives — `Esc` in the composer — and the [control strip](#the-control-strip) now shows the pace, handoff, and kill verbs as visible, focusable items one `↑` from the composer. The per-entry raw verb is on the [block menu](#the-block-menu). The single-letter transcript verb binds (`p`/`+`/`-`/`b`/`r`/`t`) have been purged (backlog/tui-control-strip/): the verbs now answer only on the strip (as scoped mnemonics or clicks) and the block menu, and a plain letter in the transcript no-ops.


## Wiring: two threads, one crossbeam channel

crossterm's event read blocks, so nothing in the UI awaits: the UI runs on a dedicated OS thread (`tui::ui_loop`) and every operator intent is *sent* over a channel that a dispatcher task on the tokio side turns into a control-channel call. Both halves of those channels are built at once and travel together (`tui::senders` — `Senders` to the state, `Receivers` to the dispatchers), so adding an intent is one field on each rather than a parameter threaded through every entry point; an offline run (the gallery, a read-only view) holds the `Receivers` unread, which is what keeps a send from erroring where there is no daemon to answer. A tokio task (`tui::bridge`) holds one connection to the unified control stream (`GET /events?include=log,output,state`, [control-api](control-api.md)) over the [transport](transport.md), decodes its frames — durable `LogEntry` entries, ephemeral `assistant_delta` / `tool_output` deltas, and whole-topic `state` replacements (a `TuiEvent::Topic` each) — into `TuiEvent`s, and forwards them over a crossbeam channel into the UI thread. There is no poll: the connect snapshot seeds every topic, and the daemon pushes a replacement when one changes. A 1 Hz clock tick in the UI loop advances the elapsed-time displays (process running/idle seconds, seconds since the last request), which derive from absolute stamps because the wire deliberately carries no ticking values.

The UI loop drains arriving events through `InteractiveState::handle_event` and sets a `reproject_pending` flag rather than re-projecting per event. Before each paint it calls `flush_reproject`, which brings the transcript up to date once and clears the flag. A reattach replay that delivers N entries therefore costs O(N) event folds and O(1) projections, not O(N²).

**Terminal input folds the same way.** A wheel notch or a held key delivers a burst of crossterm events, and the reader thread keeps queueing while the loop paints — so painting per event makes fast scrolling lag behind the wheel by however far the queue has run ahead. The loop applies the whole burst, then paints once. Two events stop the fold: a quit, and a click, which the paint has to consume before another can be recorded in its place.

### The topic store

The `state` topics the TUI reads are held in one store, `tui/topics.rs`: a `Topics` struct with a slot per topic, each the `protocol` type the host serializes for it (`protocol::state`, [control-api.md](control-api.md)) — `Option<UsageView>`, `Option<Gate>`, `LoopPhase`. A frame carries a topic's whole value, `null` when it is clear, and replaces the slot: nothing is merged, and no second type mirrors the first, so a field added to a topic's type is readable from its slot with no line in the TUI beyond the place that renders it. The topics are declared once, in a table (`TopicUpdate::decode` reads a frame by the topic's type; `Topics::replace` stores it), so a topic the TUI starts to read is one row there.

- **Absent, null, malformed.** A slot nothing has arrived for is empty (an empty list, an idle loop phase), and every reader treats that as nothing to show. `null` empties the slot. A value that does not deserialize into the topic's type is dropped with its frame, and the slot keeps what it held. A topic the TUI does not read is skipped, so a host that grows one stays usable. An enum variant this build does not know fails the whole topic's value, except where the type carries a catch-all (`RequestOrigin`, `CacheMissReason`).
- **Order.** Frames apply in the order they arrive on the one stream, each completely before the next. The host holds a `state` frame until the connection has delivered the log frames it reflects (docs/control-api.md, *Ordering*), so the store is never ahead of the transcript; the frame carries no stamp of its own, and the TUI keeps none.
- **What an arrival does beyond storing** is in `InteractiveState::apply_topic` (`state/topics.rs`). Every frame asks for a repaint. A `gate` frame also sets the status line (`approval needed: …`, or the idle line when it clears) and reprojects the transcript, because the gate renders inline at its call. An `ask` frame reprojects when the question changed and hands the recalled turns this client submitted back to the composer, once per `call_id`. A `processes` frame rebuilds the process panel's rows (the listed processes, `process_panel.rs`), each keeping the output tail collected from `process_output` deltas, which the topic does not carry. A `session_identity` frame that names a successor ends the loop toward it, unless a destination is already set. A `starting` frame that carries a failure puts the turns it hands back in the composer, once.
- **Written locally.** Two slots are written ahead of the frame that confirms them: the pace, by the keys and the stepper that change it, and the gate, taken when an approval or redirect is sent. The next frame replaces what was written.
- **Derived from the store and the clock** are kept in state, recomputed after every frame and once a second: the panel rows' running and idle seconds, the seconds since the last request (from `usage.last_request_at`), and the status bar's operation text (from `current_operations`, the loop phase and the model). The Policy section's rows are built from the `role`, `proposals` and review-level slots each time the drawer is drawn or a key resolves a row (`policy_view.rs`).


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

**Wall-clock, when a count isn't enough.** Those counts are stable and testable, which is why the tests assert on them, but they say nothing about whether a frame fits the 30 ms paint tick on a real terminal. `frame-cost = true` under `[tui]` (or `--frame-cost` on `dev gallery`, `frame_cost.rs`) times each paint and each re-projection separately — they are different fixes when one dominates: the projection rebuilds the live nodes, the paint lays out what they became. The status bar carries p50/p95/max over a rolling window beside the `laid`/`hl` counts, and the session's distribution prints on exit, including how many frames ran over the tick. Off by default, and unreachable from the snapshot harness, since a frame time cannot match twice.

**Driving the animation-rate behaviours.** `efference dev gallery --stream` plays a scripted turn — thinking, then a long file write whose JSON arrives fragment by fragment, then a command whose output streams — over the offline gallery, through the real `ui_loop` with no daemon behind it (`gallery/stream.rs`). `--chunk`/`--interval` set the pace, so both a real-model cadence and fragments arriving faster than the paint tick can be provoked. Snapshots pin layout; this is for what only exists in motion.


## The composer

`tui/composer.rs` is a dependency-free, emacs-keybind multiline editor. Its interface is deliberately swappable: `handle_key` / `render` / `text` (or `clear`). The buffer is one UTF-8 string with byte offsets for each logical-line start. The cursor, edit ranges, and visual-row boundaries are extended-grapheme boundaries; grapheme counts are derived for vertical goal-column motion. This lets combining marks, variation selectors, ZWJ emoji, and regional-indicator flags move and delete as one unit. The composer shares `render_span::wrap_points` with the document renderer; its width and hard-break traversal measure grapheme clusters with `unicode-width`, although an individual terminal can use a different emoji cell width.

`tui-textarea` was the intended first cut but is pinned to an older ratatui/unicode-width ecosystem and does not resolve against ratatui 0.30, hence the hand-rolled editor. The narrow interface makes replacing it later surgical.

**An `ask` is drawn as a turn ending** (`tui/projection/ask.rs`). A call named `ask` (docs/process-monitor.md) is not a call entry: its `question` is an assistant message whose last line carries a mark, and the call's spot, gate and result entries do not render. The committed result entry is the reply: a result carrying a `submission_id` is drawn as a user message (with the `source:` prefix a sourced `User` entry gets), and one without — an interrupt, a malformed call — as a dim `not answered` note. The mark says where the question stands. `▸ answering` (bold; `▸ answering · question n of m` when one completion put several) is the ask the live `ask` topic names — the one the composer's send answers. An ask with no result while the topic names a sibling reads `next · question n of m`, dim. Once its result commits, or when there is no topic to say more (a replay, a client that attached later), it reads `asked` / `asked · question n of m`. The topic's `call_id` travels to the projection in `Gate::ask` and is held in the topic store (`topics.ask`); the mark changes with no log entry, so the incremental projection dirties the owner of the ask the topic left and of the one it reached, as it does for a gate. While the topic is non-null the status line's idle hint (and the `click to focus this entry` hover hint) reads `your reply answers question n of m`, or `your reply answers the question` for a lone ask. The position comes from `process_monitor::ask_of_call`, so committed history and a live pending ask read the same. A call whose arguments are still streaming is not drawn while it arrives. A send while the `ask` topic is non-null is the reply, minted and echoed as any submission, and names the question it answers (`Submission::answers`, the topic's `call_id`). A send the daemon refuses — it named no question while one was pending, because the question arrived after the text was typed, or named one no longer pending — comes home as `TuiEvent::NotDelivered`: `apply_not_delivered` drops its echo, puts its text and attachments back in the composer through `apply_recall`, and the status line says `not sent: a question came first` (or `that question is no longer waiting`); the question itself arrives on the `ask` topic. Because a reply is never a `User` entry, the echo settles on any committed entry — `User` or `ToolResult` — carrying its `submission_id`; there is no settle-on-topic-clear path. When the `ask` topic carries `recalled` turns (the queue the daemon withdrew when the question began), `apply_ask` filters them to submission ids still in `pending_turns` — this client's own, minted in-process, so a restarted client restores none — and hands them to `apply_recall`, once per ask (`ask_restored` holds the last `call_id` handled). The daemon's refusal to `⏎`-continue past a question arrives as a `Notice` like any other refusal.

**`C-r` recalls a queued turn.** A message submitted while the agent is busy waits in the daemon's queue, unlogged and unprojected; `C-r` in the composer asks to withdraw it — `DELETE /turns/:submission_id`, addressed by the id the composer minted ([control-api](control-api.md)). It is a **suffix pop**: the most recent queued turn and everything after it come back together. Nothing moves locally until the daemon answers, and what it answers is what the composer gets: the withdrawn texts are prepended to whatever draft is already there, joined with a newline (adjacent pending turns are separate blocks on the wire, so popping several into one buffer has to invent a join). A pop that returns nothing is the lost race — the loop dequeued the turn first — and the status line says `already sent — nothing recalled` rather than handing back a message that is already on its way. Cancelling a queued turn outright is the byproduct: recall it and clear the composer.

**Empty `⏎` hands control back.** With no gate pending, no staged verb, and no staged attachment, `⏎` on an empty composer fires `POST /loop/continue` ([control-api](control-api.md)) wherever the `verbs` topic says the route accepts it — the zero-token continue at idle: nothing is submitted, nothing is appended, and the loop takes another turn on the wire it already has. It sends no pending echo, because nothing was said; the status line reads `continuing`. The same key is "retry now" while the loop waits on the upstream — a retry pause, or a triage held under a stall ([transport.md](transport.md)) — and the person's action that resumes a cold park, since the route accepts both. Where the topic says the route refuses, an idle loop's refusal (the wire ends where re-prompting means nothing) goes to the status line in the route's own words without a request, and mid-turn the key stays a silent no-op. Before the topic arrives, an idle loop sends and the daemon's decline arrives as a `TuiEvent::Notice`.

**A newer release sits at the status bar's right edge.** While the `newer_version` topic names a release ([update-check.md](update-check.md)), the status bar states `efference 0.2.0 is available`, dimmed, flush right inside a margin as wide as the left one (`render_status_bar.rs`). It is the one thing on the bar that is not about this session, so it is drawn only where the line has room after everything else, and a narrow terminal drops it rather than cut the session's own status. The placement is a first one.

**A cold session says what the next action reads.** While the `cold` topic is set, the status bar leads with `cache expired — a send reads ~120k tokens uncached`, in bold (`InteractiveState::cold_line`), ahead of the operator text and the other indicators so a narrow terminal cuts it last. With a gate pending the subject is `approving or redirecting`, at an `ask` it is `your answer`; `the context uncached` stands in when the topic carries no size. The line is information: a send, a continue, a decision or an answer goes through on the first `⏎`, and the host treats it as the person action that releases a parked loop ([control-api](control-api.md)).

**`C-o` compacts instead.** The alternative to the uncached read: `POST /loop/compact` ([control-api](control-api.md)) has the session's own model replace its history with a summary, keeping the most recent payloads verbatim. The header shows `compacting…` while the summary is written (a compaction in `current_operations`), cold or not. The status line reports the daemon's answer — accepted, or why there is nothing to compact — and the summary arrives in the transcript when it lands. The next `⏎` sends, and resumes a turn that was parked mid-way.

**Selection and clipboard.** The composer tracks an optional anchor alongside its cursor; a non-empty span between them is rendered with the theme selection style. Shift-modified character, word, line, and vertical movement extends from the anchor; unmodified motion collapses to the span's directional edge, and `C-x h` selects the buffer. Typing, newline, tab, deletion, and kill commands replace or delete a selection before their ordinary operation. Mouse press, Shift-click, and primary-button drag use the same wrapped visual rows as rendering, mapping positions only to grapheme boundaries.

`M-w` copies a selection, `C-w` cuts it (or deletes the preceding word when there is no selection), and `C-y` or `C-v` pastes from the system clipboard. `Super-C`/`Super-X`/`Super-V` are aliases when the terminal reports `SUPER`. `tui/clipboard.rs` owns the persistent `arboard` system clipboard and an in-process text fallback; the composer itself receives and returns plain text only. Copy and cut update the fallback before an attempted system write. Paste refreshes it after a successful system read, or uses it after an access failure. In arboard 3.6, `ContentNotAvailable` does not distinguish empty clipboard text from an unsupported format, so it neither uses stale fallback content nor labels the content type.

**Attachments.** What a paste becomes is decided once, in `tui/state/attachments.rs`, for both ways a paste arrives — a bracketed paste from the terminal (`⌘V`, or a file dragged onto the window) and a clipboard read (`Super-V`, `C-y`): a clipboard image is staged as a PNG; a paste that is exactly one or more existing absolute paths, escaped the way terminals type a drag (`/Users/me/My\ Shot.png`, `'…'`, `file://…`), stages each file; text past a dozen lines is staged as a `.txt`; anything else is typed into the composer ([inbox.md](inbox.md)). Terminals pass only text through their own paste, and an image-only clipboard reaches the app as nothing at all (kitty 0.48 sends no bracketed paste for it, not even an empty one), so a ⌘V of a copied image cannot be seen. `C-v` — like `Super-V` where the terminal reports it, and `C-y` — reads the clipboard directly: non-blank text if there is any, else the image. An empty bracketed paste, from a terminal that sends one, reads it the same way. Because ⌘V gives no sign, the strip says when `C-v` would fetch an image: `image in clipboard · C-v to attach`. It learns this **without reading the clipboard** (`tui/pasteboard.rs`) — whatever was copied last is not the TUI's to look at until the operator asks, and showing it unasked is how a private screenshot ends up on a shared screen. On macOS a tokio-side watch polls the pasteboard's change count every half second and, on a change, reads only its type list: an image type with no plain text and no file URL (a Finder file copy offers its icon as an image) is an image. Neither call reads contents or raises the pasteboard privacy alert, and neither yields dimensions, so the hint names none. Elsewhere arboard offers no content-free view, so there is no hint and `C-v` stands alone. The note clears when `C-v` takes the image, and returns only when the clipboard changes again. Bracketed paste is enabled at startup and disabled in normal and panic restoration, alongside mouse capture; without it a paste arrives as a burst of typed keys and cannot be routed. Focus reporting is enabled and disabled at the same three sites; its events and every key and mouse event feed the attention reports ([web-push.md](web-push.md)).

A staged file uploads at once (`POST /attachments`, [control-api.md](control-api.md)) on a tokio-side dispatcher, whose answer returns as `TuiEvent::Uploaded` keyed by a local staging key (`tui/staged.rs`). Staged chips sit in rows above the control strip, one per file — name, what the daemon made of it, a `✕` — capped at four with the last counting the rest; clicks resolve against per-frame `staged_hits` to the chip's key. When the terminal draws images, a staged image also shows as a thumbnail in a five-row band above the chips: the dispatcher decodes and scales it off the UI thread (`TuiEvent::Thumbnail`), and it renders from a second `ImageStore` (`staged_images`), so a staging key can never collide with a log address. `⌫` on an empty composer takes back the last chip; a chip removed mid-upload has its upload removed when it lands. The next send carries every ready chip (and waits, with a status-line note, while one is still uploading); staged chips alone are a turn, so `⏎` on an empty composer sends them rather than taking the continue. A recall re-stages the attachments its turns carried. A sent turn — pending echo and committed entry alike — lists its attachments under its text as dimmed `⎘ name · detail` lines.

**Undo and redo.** `C-/` and `C-_` undo, and `M-_` redoes. When the terminal reports `SUPER`, `Super-Z` and `Super-Shift-Z` are undo and redo aliases. History is a 200-entry bounded timeline of whole-buffer snapshots, including cursor, selection anchor, and goal column. Adjacent ordinary typing joins a burst until 750 ms of idle time, while contiguous Backspace and forward Delete runs coalesce separately. Newline, tab, paste, selection replacement, cut, word deletion, and line-kill commands each form one operation. A new edit after undo discards redo; external draft replacement and clearing reset history.

The UI requests crossterm's `DISAMBIGUATE_ESCAPE_CODES` keyboard enhancement at startup and pops it during normal and panic terminal restoration. Kitty-protocol terminals that support the enhancement can report `SUPER`; terminals that do not continue emitting ordinary key events.

**Focus model.** Focus lives on one vertical axis — transcript, [control strip](#the-control-strip), composer — and the same physical key takes a context-dependent meaning (`C-p` is "previous line" composing, "previous unit" navigating), the way an editor's motion keys don't stop existing when you leave the buffer. There is no modal toggle: you cross between contexts by *moving*. `↑` at the top of the composer (the `0,0` detent: one press lands at the start of the first line, the next crosses the edge) spills focus onto the control strip, and `↑` again into the transcript; `↓` reverses — past the last unit it stops at the strip, then falls back into the composer; `Esc` is a graduated *return to steady state* (see [Graduated Esc](#graduated-esc) below) whose first rung returns to the composer from any stop. In the transcript context every entry is a focus **unit** — each message, each thinking block, each call, each result — and the focused unit is marked by an accent glyph **`▌`** down its full height (a glyph, not reversed-video, so it shows on any theme and in the snapshot grid). `↑/↓` (or `C-p`/`C-n`) move between units; `←`/`→` choose among the focused unit's [block-menu](#the-block-menu) verbs and `Enter` fires the selected one — on most entries there are none, and `Enter` is inert. The verb keys — `r` raw, `t` theme, `+`/`-` pace, `p` pause, `b` background — are **not** bound here: after the bind purge (backlog/tui-control-strip/) each lives where it is shown (raw on the block menu, the rest on the strip), so a plain letter in the transcript is a clean no-op. The surviving letter binds are navigation and utility only (`g`/`G` top/bottom, `m` mouse, `?`/`h` help, `q` quit); typing never leaks into the composer. Submit in the composer drives `POST /turns`; submit while a gate is pending resolves the gate (below); `C-d` on an empty composer quits (shell EOF).

Beside this axis sits a second *column*: the [session drawer](#the-header-and-the-session-drawer). `Tab` in command mode (transcript or strip focus) opens it with focus and closes it back to where focus left; `Esc` still homes to the composer from there. In the composer `Tab` is a typing key — a tab enters the draft as the four spaces every render path displays it as — so entering the sidebar never steals a key the composer wants.


## The control strip

A one-row surface between the transcript and the composer (`tui/control_strip.rs`), always present and never empty: the currently relevant **loop verbs**, rendered as focusable, clickable, button-like items. It is the discoverable slow path and the keybinding legend in one place — each item shows the key that fires the same verb without visiting the strip.

Contents are **one relevance-ordered list per state** — the state's context verbs, then an omnipresent tail — filled to the available width and cut from the right. So `↑ ⏎` from the composer always lands on the most relevant verb, and what a narrow terminal loses is always the least relevant one. Context verbs by state:

| state | context verbs (most → least relevant) |
|---|---|
| idle | *(none; `resume` when held)* |
| start failed | `retry r` (*A session that is starting*, below) |
| gate pending | `approve ⏎ · abort`, with a dim `type below to redirect` note |
| running — foreground `bash` | `background b · kill ⎋⎋ · pause ⎋` |
| running — any other tool, or still generating | `interrupt ⎋⎋ · pause ⎋` |

`held` is a modifier rather than a state of its own: wherever the hold verb appears it reads `resume p`. A hold set mid-trajectory outlives the turn, so `resume` stays reachable at idle; there is nothing to pause when nothing is running.

The tail is `pace <v> · sessions s · theme t · help ? · detach q`, dimmer than the context verbs and always last — which makes it exactly what the cut takes first, right to left. Every tail verb has a keyboard fallback, and the order protects `pace` most (it is a control, not a setting) and `detach` least. `detach` leaves the TUI while the daemon and session keep running; in the TUI that launched an island it reads `quit`, since the host ends with it ([operating.md](operating.md)). `sessions` opens the session switcher (below), which is where the verbs that act on a session's *life* — switch, and end — live; keeping them behind one doorway is why `detach` still has no terminate twin beside it on the row.

Only verbs that apply in the state are shown: the strip flashing between states is preferred to carrying an affordance that would do nothing. `background` is **foreground-`bash`-only** for that reason — the handoff adopts a live child into the process monitor, and there is no child to adopt anywhere else.

`kill` and `interrupt` are **one verb** (`StripVerb::Interrupt`, `POST /loop/interrupt`) under two labels, because it keeps two promises and only one of them is universal. Everywhere, it stops the loop at the next juncture and withholds the in-flight call's output from the model ([control-channel.md](control-channel.md)). On a foreground `bash` it *additionally* kills the process group — the stronger promise, and the one worth naming, so there it reads `kill`. Elsewhere — an MCP tool mid-call, or the model still generating — it reads `interrupt`. It is shown in every active state: without the latch it is a button that does nothing outside bash (nothing awaits the abort future during a synchronous MCP call), leaving the strip with no interruption verb to offer there.

One transient sub-state owns the row outright and carries no tail: the active pace stepper (only the loud `▴ pace <v> ▾` and its note), described under item classes below.

Because the item list is a pure function of the state topics plus the committed log, re-ranking happens only at state transitions — and, while the strip is engaged, not even then (below).

**Focus is a `StripVerb`, never a position.** The list is rebuilt each frame with the context verbs *prepended* to the tail, so every position shifts when the state flips: index 0 idle is `pace`, index 0 with a foreground bash is `background`, index 0 with any other active call is `interrupt`. An index-held focus therefore becomes a *different verb* between the focus landing and the `⏎` — observed live, where a move to activate `pace` on an idle loop interrupted a running bash command instead. Focus, the click hit-map, and the drawer's return-to-strip all hold the verb; the index is re-derived per frame for the `←`/`→` walk alone. A verb the state stopped ranking dispatches **nothing** and says so on the status line, rather than firing whatever moved into the slot.

**Engaging the strip pins the item list.** The moment it takes keyboard focus, or the pointer enters its row, the *order* freezes: verbs that stop applying grey in place instead of being removed, so no position moves under the operator's hands or their cursor. Labels keep tracking the live state (a hold set elsewhere turns the pinned `pause` slot into `resume`), because a frozen label is exactly the stale information the freeze exists to prevent. The row says it is pinned — a dim `pinned · leave to re-rank`, shown whenever the pinned order has diverged from what the state now ranks, and taking precedence over the gate's own note: a silently frozen strip would be a new way to act on stale information, which is the same defect class as the shifting one. Nothing is pinned while the pace stepper owns the row, since it is a single widget rather than a ranked list. The list re-ranks when keyboard focus and hover have both left.

Two consequences, taken deliberately: a verb that becomes *newly* relevant while the strip is engaged is invisible (and its scoped mnemonic inert) until the operator leaves — the strip shows what it shows; and a pointer that leaves the terminal without a leave event holds the pin until keyboard focus resolves it, since level-triggered hover is all a `Moved` event stream provides. Both are recorded as remaining design in `backlog/inbox/strip-verb-misfire.md`.

**Which tool is running** comes from the log the TUI already holds, with no daemon round-trip: the loop appends the assistant entry *before* running its call and the `tool_result` after (`src/agent.rs`), so the last call with no paired result is what is executing now (`in_flight_call` in `tui/state/lifecycle.rs`). The call is read through the log's dialect-neutral accessor, so a Responses `function_call` is found the same way a Messages `tool_use` is; an entry whose argument string does not parse names nothing, since the loop records such a turn without executing it. A completion may make several calls ([README.md](README.md)), and their results are appended together after the gate, so on a multi-call entry this reports the entry's first call for the whole step — exact for a single call, a stale label while a later call of the batch runs.

The focused item fills a reserved marker cell with `▸` (a space when unfocused), so focus movement never shifts text and stays visible in the character-grid snapshots; the raised-surface color change is what pixels show, asserted in tests via `cell()`. A pinned item whose verb no longer applies keeps the button surface and drops to the muted foreground plus `DIM` — inert, and distinguishable from a tail verb, which the grid can't show but `cell()` can. `←`/`→` (or `C-b`/`C-f`) move between items, `⏎`/space or a mouse click activates — clicks resolve against per-frame recorded column ranges (`strip_hits`, which map a column to a *verb*: a click has no dwell in which to notice the row changing). Activation dispatches onto the same mechanisms the keybindings use (the pace and interrupt channels, gate approval); `detach` raises a quit flag the UI loop checks alongside the key handler's return, since clicks have no quit return path. A state's dim note has its width reserved out of the item budget before any item is placed — an instruction is never what the cut drops — and is then drawn adjacent to the last item that fit. The strip is chrome: never projected, never in the log.

**Scoped mnemonics.** While the strip is focused, a plain letter that matches a *shown* item's hint fires that item (`b` → background while a foreground command runs, `t` → theme, `p` → resume while held), wherever the selection currently sits — a shortcut, not a focus move. This is the only surviving letter-form of the purged verbs (backlog/tui-control-strip/): the letters answer only where their item is on the strip, never as buried global binds, so `p` does nothing from an unheld strip (which shows no resume) and pausing a *running* loop is Esc, matching graduated Esc. Items whose hint isn't a bare letter (`⎋`, or an empty hint) are driven by their own key or the arrows. An unmatched key falls through to the remaining transcript keys (scroll, help, quit).

**Item classes.** Most items are *buttons* — `⏎` fires the verb. The second class is a *widget*: a verb whose value is adjusted in place. The tail's `pace` item is one — `⏎` enters an inline stepper (`PaceWidget` in `tui/state.rs`), rendered loud as `▴ pace <v> ▾` (the `▴`/`▾` are the grid-visible "stepping" tell, the way `▸` marks focus), with a dim `↑/↓ adjust · ⏎ set · ⎋ revert` note. While active, the stepper owns the row — every other item leaves, so nothing else can be focused or reflow behind the changing value — and it alone captures `↑`/`↓` (`↑` climbs toward more between-turns delay). Each step **previews** by mutating the live pace (the status bar tracks it) without pushing to the daemon; `⏎` commits the previewed speed over the pace channel and leaves, and `Esc` reverts to the speed at entry — the gentlest rung of graduated Esc, and since previews were never sent there is nothing to unsend. It replaces the earlier `slower`/`faster` buttons, so between-turns pace is adjusted only here — and since `pace` is in the tail, it is reachable in every state.


## The session switcher

`sessions s` opens an overlay (`tui/sessions.rs`, painted by `tui/render_sessions.rs`) holding the host's sessions: a row per session with the id, status, title, background-process count and a marker on the one this client is attached to. The rows come from `GET /sessions` ([control-api.md](control-api.md)), asked for when the overlay opens and again after an end; the cursor holds the row's **id**, so a row leaving the listing does not silently re-aim a destructive key at whatever slid into its place.

`⏎` **switches**. The TUI streams exactly one session, and the per-session contract is one cursor per handshake, so a switch is not a second subscription: the UI loop returns a destination, the entry point (`tui::run`) tears the attach down — aborting the bridge drops the SSE stream, which closes the connection — and re-attaches with the client re-based onto the new prefix over the same transport. `⏎` on the attached row closes the overlay instead, since re-attaching where the client already is would only cost the transcript its scrollback.

The hand-off call itself draws its brief under "to the new session:" as prose rather than as a code-style argument, so it does not read as the agent addressing the operator. A session that hands off (docs/tools.md) is followed: when its `session_identity` names a successor, the TUI leaves by the switcher's own exit and attaches to it, and the successor's transcript opens on the predecessor's brief, drawn as a harness annotation ("⟢ handed off from `<id>` · its brief:") rather than as the operator's turn. `r` opens the **resume page** as a modal over the listing, the way `n` opens the form: archived sessions under the TUI's working root (`GET /sessions?state=archived&cwd=<root>`, [control-api.md](control-api.md)), read through a host command when the key is pressed, with a "reading…" line until they land and the host's error as the note if the read fails. The page is `tui/resume_list.rs`, the same state, filter rule, keys, and rows the bare-`efference` picker's resume page draws (`bin/efference/picker/app.rs`): title, dim id, dim age, a query line filtering on title and id, Esc clearing a non-empty query first. It owns every key while open, Esc and Ctrl-C included; Esc on an empty query closes it back to the listing. `⏎` on a row sends `PATCH /sessions/:id {"state":"active","cwd":<root>}` and shows `resuming <id>…` until the host answers; on success — once the host has the session starting — the TUI switches to it exactly as `⏎` on a live row does, and a refusal leaves the page with the host's message and the same row to try again; a resume that fails while starting is shown on the session's own view (*A session that is starting*). `a` **archives**, and is the only destructive thing the TUI can do: the session stops and leaves the list, and its log stays on disk for a later resume. `DELETE /sessions/:id` reaps the session's background processes and removes its scratch, and the host answers only once that teardown has run — so the confirm exists, and its rule is the listing's own two fields: a session that is doing something (`running`, `gated`, `asking`, `stalled`, `parked`) or that holds background processes is confirmed, naming which; an idle session with nothing running is archived on the first press. While a confirm is up it owns every key — `y` or `⏎` goes through, **anything else cancels** — so a mistyped key can only ever back out. Archiving the attached session ends its event stream, which the bridge reads as `TuiEvent::Disconnected` and the UI loop leaves as `Exit::Closed`. The entry point (`tui::run`) restores the terminal first, then asks the host what became of the session (`tui/closed.rs`: `GET /sessions/:id`, asked again for a few seconds while the answer still says the session is running, because the stream ends before the archive finishes; a session leaves the host's list only once its `Archived` entry is written, so one found on disk without it is reported at once) and prints one stderr line: `efference: <id> was archived by <source>; its log stays at <cwd>/.efference/<id>/`, the `by …` clause present only when the `Archived` entry records a `source`; `efference: <id> stopped when the host did; its log stays at …` for a session on disk with no `Archived` entry last; `efference: <id> is no longer on the host` when the host knows no such session (one that never had a turn is discarded, not archived); `efference: the stream for <id> ended while the host still lists it as <status>` when the host is still running it; and `efference: lost the connection to the host at <socket>` when the host does not answer. A session that failed to start keeps its view open and prints nothing (*A session that is starting*). The TUI's own archive (`a` in the switcher) sends no `source`, as the TUI sends none on an interrupt, so a session archived from another TUI reads without the `by` clause.

Esc closes the overlay, and a confirm it was holding goes with it. That is the direction Esc is allowed to move a destructive verb.


## The block menu

The strip carries *loop verbs* (act on the daemon, matter mid-composition); the **block menu** (`tui/block_menu.rs`) carries the *entry verbs* of one transcript unit — the actions on the entry itself. It travels with `↑`/`↓` focus and renders trailing the focused unit's header line (`✓ 20 lines  ▸ raw`), styled like the strip's items: a reserved marker cell (`▸` selected, a space otherwise, so moving the selection never shifts text) and the same button surface. Trailing the header rather than taking its own row means moving focus never reflows the transcript — a dedicated row would repaint on every `j`/`k`.

The verbs a unit carries are a pure function of the entry (`block_menu::menu_items`), computed once and read by two callers with no drift: the renderer (which has the node in hand) and the key/mouse dispatch (which looks it up by focus). Two verbs so far, one per entry kind that has a fork behind it:

- **raw / triaged** — the `toggle_raw` flip (docs/triage.md), on a keyed `ToolResult`. The new home for the forgettable `r`.
- **full / peek** — the `toggle_expanded` flip, on a keyed `RunningTool`: read the whole output that has arrived instead of its trailing lines. Reachable only while the call runs; the committed result renders whole and offers the raw fork instead.

**Most entries carry no verbs at all**, and that is the ordinary case rather than a gap: no other per-entry mechanism has been earned yet. `⏎` on an empty menu falls through to a plain activation, which for a transcript entry does nothing. Building the affordance before knowing what it should do is what produced a truncation default in the first place.

`←`/`→` (and `C-b`/`C-f`) choose among the items, clamped at both ends; `⏎`/space or a click fires the selected one — clicks resolve against per-frame document-coordinate rects (`block_menu_hits`) *ahead of* the whole-header focus action underneath, or every menu click would just move focus. The selection resets to the leading verb whenever focus moves to another unit. A gated call has no result entry at all until it resolves, so nothing there carries a raw flip; its verbs are the gate's, on the strip. Later entry verbs (full-screen, fold, open-in-editor, actions on a backgrounded process's entry) attach here as their mechanisms land.


## Inline approval gates

The approval gate is ephemeral control state: the daemon gates *after* appending the `tool_use` to the log and *before* running the tool. The transition arrives as a `gate` state frame — the topic's whole value when a gate registers, `null` when it resolves — stored as `topics.gate`. Delivery is coherent with the log ([control-api](control-api.md)): the frame never precedes the entries it reflects, so the gate renders against a transcript that already shows its call.

The gate renders **inline at the pending call**, not as a bottom banner. Because the loop appends the assistant `tool_use` before it gates and one tool call runs per turn, the pending call is always the tail call with no result. `project` receives `pending_call` + `pending_reason` and emits the gate beside that call, where the result will land — the spotter's reason (when it gated via the spotter — read from the `Spot` audit entry, see [permissions](permissions.md)), then the affordance carrying the `⏸`:

```
› rm -rf /tmp/scratch
spotter gated · deletes outside the git tree — not reversible
⏸ awaiting your approval  ·  ⏎ approve · type to redirect
```

Reading order is call → what the spotter made of it → what to do, so the affordance lands adjacent to the strip carrying the verbs.

When the held call is a session's allowing `agents__decide`, the gate shows the child's move in place of the handle ([permissions](permissions.md)): the logged call line stays as the model wrote it, and an annotation below names which child and what it is holding. The chain joins with `→` when the approval passed through more than one session.

```
› agents.decide(agent: "child", decision: "allow", seq: "4")
⤷ acting on child's bash · rm -rf ./build — clean the build tree
⏸ awaiting your approval  ·  ⏎ approve · type to redirect
```

**Every judged call carries its verdict, not only a held one.** The `Spot` entries are indexed by `call_id` alongside the results and emitted with the call they judge, so a *cleared* call shows its reason too — which is most of them, since clearing is the common verdict and nothing ever interrupts for one. The stance leads the line (`spotter cleared · …`, `watching`, `gated`) because it is the scannable part; without it every verdict looks alike. The whole line sits in the dim register: before the decision it is why the call is held, and after it is a gloss on what the call was taken to be, so it never competes with the call or its result for attention. A resolved gate keeps its verdict, which is the only surviving record when a human approves something the spotter wanted to stop. `Origin` is not drawn — a fail-closed verdict's own reason reads "spotter unavailable", so a glyph would only repeat the line. The committed entry is the source; `pending_reason` from the gate covers only a client holding the gate without the `Spot` entry behind it — a window the stream's coherence rule closes (a `gate` frame never precedes the entry it quotes, [control-api](control-api.md)), so the fallback survives as robustness rather than as a path that renders (it has no stance, and reads `spotter · …`).

Whether this much per-call annotation earns its space is an open question the dogfooding loop is meant to answer; a suppression setting is the obvious lever if it turns out to be noise.

**What the harness says about a call is its own entry, never inside the call.** An `Annotation` entry is a different rung's judgment *about* the call rather than part of the call's content, so it sits beside it — and, not being agent-generated, it is not a focus stop. It is read in place, not visited. The reading order call → verdict → affordance/result comes from entry order alone, and the no-blank-line rule (`starts_group`) is what makes the run read as one unit.

The split was originally forced: while the reason and the affordance were nested inside the exchange body, a multi-line command filled the collapsed body on its own and clipped both, and the `⏸` was replaced by the disclosure triangle the renderer substituted once a body was collapsible — so a gate on any realistic command showed no reason, no affordance, and no sign the call was held. That truncation is gone, but the split is kept on its own merits: the `⏸` belongs to the gate, which is something the harness did, not to the call.

The status bar mirrors this affordance for the scrolled-away case, and the [control strip](#the-control-strip) simultaneously carries the full verb set (`approve ⏎ · abort`), so `↑ ⏎` approves without typing and the strip — not instruction text — is the legend for abort. Strip `approve`/`abort` resolve the gate directly *without* clearing a drafted redirect the operator decided against sending (unlike an empty-composer `⏎`, which routes through the submit path below).

The composer composes text only. Submitting from it while a gate is pending resolves the gate with the `Approval` the loop consumes (from `src/record/log/approval.rs`):

- empty → `Approval::Approve`
- any text → `Approval::Redirect(text)` — including text that begins `.abort` or `.allow`, which are sent as written

Abort is the strip's `abort` verb. The TUI offers no grant at a gate; a grant is committed over `PUT /approvals/:call_id` with `config_edit` ([operating](operating.md)).

Resolving replaces the inline affordance with the real result entry (or, on redirect/abort, an error result), and the call stays in the transcript with that outcome in place of output.

## The header and the session drawer

The top row is the **header** (`tui/render_header.rs`), modelled on the web client's top bar ([web-client](web-client.md)): the `efference` wordmark, the session's id and title on the left; on the right `cache expired · C-o compacts` (`· compacting…` while `current_operations` holds a compaction; `paused: cache expired · ⏎ to continue · C-o compacts` when the `loop_phase` topic says the loop is `cold_parked`; `paused: summarized · ⏎ to continue` for a park that outlived a fold, which is `cold_parked` with `cold` null; `C-o compacts` is shown only while the `verbs` topic says the compact route would take a request without `warm`) while the `cold` topic says the cache is believed expired ([control-api](control-api.md)), the cache-misses warning `⚠ cache misses ×7` when `usage.cold_streak` — unexplained cold reads in a row — is past its threshold ([log-and-projection](log-and-projection.md)), a ten-cell context meter with its `183k/1m` caption from `usage.context`, the session cost from `usage.cost` ([transport](transport.md)), and a **session** button. The button has a hover state (the strip's focused style) and holds it while the drawer is open; its column span is recorded per frame (`header_hits`) and a click resolves against it. The transcript starts on the row below.

The **terminal title** — what a terminal shows on the window's tab — names the attached session: its title, then its id (`Kitty tab titles · amber-heron`), or `efference · <id>` before a title is written ([session-title.md](session-title.md)). The UI loop rewrites it when a `session_identity` event changes it, and drops control characters first, since a session title is model-written and an ESC or BEL inside it would end the OSC early. Each attach saves the title it found on the terminal's title stack (XTWINOPS 22) and restores it (XTWINOPS 23) when it ends, as does the panic hook, so a quit hands back the shell's title; a terminal without the stack ignores both. `src/platform/terminal_title.rs` holds the sequences and the formats, shared with the host's own terminal title ([operating.md](operating.md)).

On a terminal wider than `TRANSCRIPT_MEASURE` (112 columns) the transcript, the strip, the staged chips, the process panel, and the composer share one centred column, as the web client centres its page; the header and status rows keep the full width, the scrollbar the screen's last column, and the drawer the right edge. The margins are painted with the document background. Hit maps that record screen columns (the strip's items, the staged chips' `✕`) add the column's offset.

The transcript renders at the origin of a buffer of its own and is copied into the frame at its offset (`render_frame`): every layout and paint path works in document coordinates anchored at `(0, 0)`, so chrome above or beside the transcript is a change to the copy offset, not to the renderers. The offset is kept as `viewport.doc_origin`, and the mouse handler translates pointer positions by it before they meet document coordinates. Cells are copied whole, including an image protocol's escape cell and the skip flags beside it, so native images land where they were laid out.

The **session drawer** is what used to be a docked right sidebar: closed by default at any width, opened by the header's button or by `Tab` in command mode, closed by `Tab`, `Esc` (the first rung of [graduated Esc](#graduated-esc) — an open drawer is never "at rest in the composer", so `Esc` cannot reach the pause or interrupt rungs while it is up), or a click anywhere else in the transcript. It overlays the transcript's right side and only the transcript rows, so the strip, composer, and process panel stay whole beneath it. It lays its contents out the way the web client's drawer does — bold section titles, muted labels in an 11-column gutter, values beside them: **Session** (the title, wrapped to two lines; `model`, and on a hosted session `spotter` and `triage` named by connection; `directory`, `$HOME` shortened to `~` where the drawer draws it; `session`, the id — from the `session_identity` topic, see [control-api](control-api.md) and [session-title](session-title.md)), **Processes** (the live background processes), **Usage** (`context` occupancy, the cache-misses warning, `cost` — with an unpriced count where the provider missed requests — `tokens`, triage retention as `kept 55% · 13k/24k lines` from `usage.retention` per [triage](triage.md), cache reads, idle time), and **Policy**. It is chrome, fed by the `session_identity`/`usage`/`processes`/`role` state topics (`tui/render_sidebar.rs`), with a `│` dividing it from the transcript. Its `Rect` is remembered for click dispatch, which resolves drawer clicks in screen coordinates before anything underneath, and so `Alt-S` can screenshot just those columns. The background-process panel above the strip shows the running processes whether or not the drawer is open. The status row carries no usage readout: the header's meter and the drawer's Usage section replace it.

**Policy** renders the **review level** (`/state`'s `review_level` — who reviews a gated call, read-only here; set it with `efference control review-level`, the interactive gear selector deferred to the ergonomics pass) followed by the effective perimeter — scope (wrapped to three lines), commands, paths, tools — read from `/state`'s `role`. Pending **proposals** (from `/state`'s `proposals`: prefix-promotion, a session's config-write) render *in the same section, in their committed sort position*, flagged with a trailing `⊕` in a muted style — a proposal is a projection of its committed self, so committing or cancelling it drops the `⊕` in place without reflow — from the focused row's verbs (below), or over `PATCH /proposals/:seq` by any other client. This is the human/orchestrator surface for the config-edit loop (see [permissions](permissions.md)).

**Drawer focus** (`tui/sidebar.rs`). Opening the drawer brings it into the focus model — from the header's button the composer gives up focus first, since the drawer takes the keys while it is up: `↑`/`↓` walk the *actionable* rows — processes first, then proposals in their committed sort position; section headers, `SESSION`, `STATS`, and committed grants are readout and skipped — clamped at both ends, since the column is not an axis stop. `←`/`→` choose among the row's verbs (`view`, `kill` on a process; `commit`, `cancel` on a proposal) and `⏎` fires the selected one; Tab again returns focus to where it entered, Esc homes to the composer. A focused row carries the transcript unit marker **`▌`** in the pad column beside the divider and its verb menu trailing the row's own line — the block menu's grammar and reserved-marker-cell rule, one layer over — so focusing never reflows the column. Rows are held by **identity**, never screen position: a process by its id, a proposal by the `seq` of its `Proposed` entry, and every activation re-resolves against current data. A row that vanished between focus landing and `⏎` — process exited, proposal resolved by another client — no-ops with a status note and leaves the column open-but-unselected rather than firing whatever occupies the position now; the next arrow key re-selects from an end. Verbs dispatch over their own channel (`tui/run.rs`) to the tokio side, which issues `DELETE /processes/:id` or `PATCH /proposals/:seq`; clicks resolve against per-frame recorded rects like the strip and block menu.

**`view` on a process row** jumps transcript focus to the entry the process came from, resolved through the call↔process mapping the daemon carries on each process (`call_id`, [process monitor](process-monitor.md)). The landing target is the entry keyed by that `call_id` — the tool result, which for a backgrounded command is the handoff result naming `p#<n>`, directly under the call that issued it. It closes the drawer, lands the keyboard cursor on that unit, and scrolls it into view; focus is all it does, and the unit's own block menu is one `→` away. Unlike the other row verbs it sends nothing to the daemon — it is this client's focus and nothing else — and it leads the row's verbs so a bare `⏎` on a process reads rather than sending SIGKILL. A process the daemon reported without a `call_id` carries no `view` verb at all, and an entry outside the transcript window the client holds is a status note rather than a jump to some other unit.


## A session that is starting

Creation and resume answer once the host has reserved the session, so the TUI attaches
while the session is still being prepared ([control-api.md](control-api.md), "A starting
session"; `tui/starting.rs`). The `starting` topic is held in the store, and while it is set
the status line leads with what preparation is doing — `starting — running the env hook`,
`starting — loading the config and mounting MCP servers`, or `starting — waiting for
`<server>` to be authorized in a browser on the host` — in the slot the cold line takes on
a started session. The composer works as anywhere: a turn sent now is queued and is the
first the session runs, and the `verbs` topic refuses `⏎`-continue and `C-o` with the
host's message.

A failure keeps the view. The turns the failure hands back go into the composer the way a
recall's do, the status line reads `did not start: <reason>`, and the stream ending after
that frame does not close the TUI as a disconnect otherwise would. The strip's one context
verb is `retry`: for a resume it sends the same `PATCH` again and switches back to the
session once the host has it starting; for a creation it opens the new-session form, and
the session that form creates is attached with the composer's text carried over
(`Exit::SwitchWithDraft`), so the turns reach the session the retry made.

## Iterating the display without being blind

`tui/harness.rs`'s `Screenplay` (test-only) scripts a transcript through the *real* event and input handlers, renders to a ratatui `TestBackend`, and frames the buffer as a ruled character grid pinned by an `insta` snapshot — so a wrap or layout change is a legible diff. The grid drops color by design so that `cell(x, y)` / `find(needle)` assertions can check foreground styles and modifiers (diff green/red, error red, the user's blue accent gutter) that a snapshot alone cannot show. Keys route through the full `handle_key_event` (`Screenplay::route_key`), so focus transitions — the spill, Esc-home, unit navigation — are exercised as production code, not a synthetic shortcut.

Two facilities close the gap between *what a snapshot shows* and *what is actually reachable*. The grid encoder is one shared module (`tui/grid.rs`: `frame_grid`, and `frame_grid_region` for a sub-rectangle); a **dev screenshot keybind** (gated by `EFFERENCE_DEV`) runs it against the *live* `InteractiveState` and writes to `.efference/screenshots/` — `Ctrl-S` captures the full screen, `Alt-S` just the session drawer's columns, opened for the capture (a narrow capture of a wide terminal) — so a live capture is byte-comparable to a committed `.snap`. And `Screenplay::replay` feeds a real recorded log (`persist::load` of an `.efference/*.jsonl`) through the same handler, so a snapshot taken at index *n* is a state the daemon actually produced — reachable by construction, not hand-authored. A trimmed real session lives in `tui/fixtures/`.

The two oracles answer different questions and neither substitutes for the other: the character grid is the **layout** oracle (wrapping, columns, focus geometry — a legible diff), and **pixels** are the appearance oracle. Appearance review happens outside the repo: drive the real TUI (`efference dev gallery`) through a `vhs` tape to a PNG. A harness-only state that `gallery` can't reach has no pixel path yet — that is what a `Buffer → ANSI` encoder beside `frame_grid` would buy (see [`../backlog/inbox/tui-polish.md`](../backlog/inbox/tui-polish.md)).


## Lifetimes

The IR keeps ferritin's `Cow<'a>` borrowing, used as `Document<'static>` over owned strings. Nodes are replaced wholesale rather than edited in place — the incremental projection splices a rebuilt entry's nodes over its old ones — so the borrowing costs nothing here, and staying byte-identical to ferritin keeps improvements backportable both ways.


## Backlog

Remaining polish — the box→`│` connector refinement for result bodies, bring-to-foreground on a process row (no daemon transition exists to drive it), the narrow-mode scrimmed drawer, the at-rest unit boundary, and refreshing the help-screen keymap text — is tracked in the backlog.

## New-session form

One form creates sessions from two places (docs/control-api.md):

- **Before a session exists** — the bare-`efference` picker's "create a new session", the
  picker with nothing to pick, a terminal island, and the setup offer (docs/operating.md)
  all open it in an inline viewport, and the client attaches to what it creates. An
  island runs it against its own freshly started host. `efference control create` is
  the non-interactive path.
- **From inside an attached session** — `n` in the Sessions overlay opens it as a modal.

The embedded one does not open on the keypress: `n` sends `HostCmd::NewSession`, and the
reply — the working root a session would be created in, plus `GET /inference`'s connection
rows — is what opens it. The inline one reads the same context *before* taking over the
terminal, because ratatui fixes an inline viewport's height at creation and the height
depends on how many connections there are. Either way the form never displays a root or a
connection list the creation would not actually use.

Both talk to the host through `host_dispatch.rs`, a task holding a client re-based at the
socket root. It is a module rather than a closure inside the full-screen loop precisely
because the inline container has no `ui_loop` and still has to preview and create, and one
dispatcher is what keeps the two surfaces from drifting.

Four host-owned inputs meet in the form and nowhere else: the connection inventory,
the model catalog, the effective draft `QUERY /sessions` recomputes, and the creation
it finally posts.
`session_form.rs` holds the operator's raw overrides and the host's latest answer about
them side by side. The answer never replaces the draft — folding resolved values back in
would turn every inherited default into an explicit override on the first preview — and
creation sends the overrides, not the resolved values.

Every field renders its value and the precedence branch that supplied it, because an
inherited default and an explicit override display the same string otherwise. A field the
host flagged is a third case with neither a value nor a source, and its message replaces
the source column. `Start session` is refused while the host has not answered, or has
answered with issues: posting a creation the host is known to reject spends a round trip
to learn what is already on screen.

The one field with no host-resolved value is **worktree** (docs/worktrees.md): a
client-only toggle — `⏎` on its row flips it, `r` puts it back off — whose choice creation
sends outside the options a preview resolves, so neither gesture previews. Its dim trailer
states what turning it on does rather than where a value came from, and it follows the
value rather than sitting in the source column, so the description is not padded off the
modal's capped width. The row is drawn only where the preview's `worktree` answer is not
`false` (the directory is a git work tree whose `HEAD` resolves; the row shows until the
first answer lands): where the host says it cannot start, the row is neither rendered nor
reachable by the cursor, and the draft's toggle is cleared so creation does not send it.

Preview is recomputed on *committed* edits — leaving a picker, accepting an editor, `r`
resetting a field — never per keystroke, so the request rate follows selections and no
debounce timer is needed. A reply that arrives after the draft moved on is dropped and the
current draft re-sent. Opening resolves the untouched draft, so the form shows the defaults
a session would actually get rather than blanks.

The working root and environment are the invoking client's, captured on the tokio side
in `host_dispatch.rs`. The root is displayed, not edited: editing it alone would pair it
with an environment captured elsewhere. The TUI supplies this environment explicitly;
the host's user-wide environment hook applies to requests that omit it (docs/env.md).

Each role's model is chosen in one picker over the whole catalog tree
([model-catalog.md](model-catalog.md)), a typeahead rather than a chain of menus. The
tree has connections (interior), groups (interior, only where the provider's rows carry
a `group`: OpenRouter's authors), models (leaves), and under a connection chip a
synthetic typed-id leaf. The input line is the path: chips for the connection and group
taken so far, then the query, as in `[openrouter] [z-ai] gl▏`.

What the rows are depends on the path and the role. The root with nothing typed lists,
for the main role, the top five recent picks across connections (as
`connection/model`, trailer `recent`) and then every connection; the spotter and triage
roles list the connections alone, since history records only main picks. Under a
connection chip the main role lists that connection's top five recents first, then its groups by
name (trailer `N models`) or, for a provider without groups, its models in catalog order;
under a group chip, its models. A connection row shows its provider and, when relevant, a
trailer: `unavailable` (dimmed, still selectable, since a typed id is still possible and
preview names the problem), `loading…` while a connection with no copy is being read, or
`failed`. A model row shows its id relative to the chips (the `author/` prefix is dropped
under a group chip), the name when it adds information (its lowercased alphanumerics
differ from the id's last segment), and up to three dim facts: context (`1M`, `272k`),
price (`$0.15/$0.50`, `free` at zero), and age from `created` (absent when unset; `3d`, `5w`, `8mo`, `2y`). Columns align
across the visible rows; at narrow widths the facts go first (age, price, context), then
the name, then the id is truncated.

**The one-connection root.** When the role can choose from exactly one connection
and it is not unavailable, the untouched root stands for that connection's own level
(`Form::picker_level`): the same recents, then the same groups or models, with no
connection row, and a group row descends to the connection's and its own chip as it
does under the connection chip. Typing at that root keeps the root's search (slugs
stay `connection/model`) and offers the "use `<typed>` as model id" row for the
connection. The breadcrumb stays empty, Backspace has nothing to remove at it, and
the placeholder and the status line (`reading <connection>'s models…`, a failure's
reason) read the connection's level. An unavailable only connection keeps its row at
the root, where its reason shows.

With a query, the rows under the chips are filtered and sorted by the matching rule in
`matcher.rs`: case-insensitive, whitespace-separated tokens that must all match. A token
without `/` is matched against the row's own text (a connection's or group's name, a
model's id without its author with a leading `~` alias marker kept, and the model's
`name`), so a row is not found through its ancestors' names: `op` at the root lists
openai and openrouter, not every openrouter model. A token with `/` is matched against the
row's slug relative to the chips (`z-ai/gl`, `openrouter/z`). Each token scores a tier
(exact final `/`-segment; prefix of the text at a segment start, where segments break on
`/ - . : ~`; substring of the text or name; subsequence of the text). Rows sort by their
worst token's tier, interior rows ahead of leaves within a tier, then (main role only)
recent rank, then catalog order. The rule is one the web client implements too, and both
run the fixture `session_form/match_cases.json` (a small tree and a list of
`(chips, query) -> ranked ids`). Under a connection chip, a non-empty query without
whitespace adds a last row, "use `<typed>` as model id"; it is the only row when nothing
matches, and is present when something does because a wanted id can be a prefix of a
listed one. Under a group chip a bare typed id is prefixed with the group, since the chip
already names the author. An empty result reads "nothing matches" (or, with no
connections at all, "no connections read from this host").

Keys: printable characters edit the query (the composer's editing binds apply); Up/Down
and C-p/C-n move the selection, which returns to the first row whenever the query
changes. Tab or `/` on an interior row pushes its chip and clears the query (`/`
inserts a literal slash where the highlighted row is not interior); Enter does the same
on an interior row and picks on a leaf, setting the role's draft and asking for a
preview. Tab on a leaf pushes its ancestor chips (connection, then group) and fills the query with
its own id (`~` alias marker kept) so a variant can be typed.
Backspace on an empty query removes the last chip whole. Esc clears a non-empty query and
otherwise returns to the form; `r` on a form row resets that role. There is no inherit row:
the form's `r` is the reset.

The form reads `GET /inference/catalog` when it opens (stored copies, never waiting on a
provider), then the dispatcher awaits `GET /inference/catalog/:connection` for every
entry that is not `fresh` (and not `unavailable`), one task per connection, and each
answer replaces that connection's entry in the open form as a `TuiEvent::Catalog`; the
selection stays inside the rows that are there. A query under a connection chip that
matches nothing asks for one `max_age=60` read of that connection, at most once per
connection per visit to the picker, so a model released since the last read shows up by
the time its id is typed. Both containers forward that request (`Action::RefreshCatalog`
to `HostCmd::Catalog`), and a failed read marks the entry `failed` and keeps its rows.

Creating switches to the new session as soon as the host has reserved it, the same
one-cursor-per-handshake switch `⏎` performs; the session's preparation shows there
(*A session that is starting*, below). A refused creation keeps the form open with its
edits and the host's reason, because nothing was reserved to reconcile with.

The appearance is shared with the bare-`efference` session picker
(docs/operating.md), which is built on the same pieces: `inline.rs` (raw mode, the inline
viewport, and the TUI theme's foregrounds as the prompts' styles) and `inline_list.rs` (the
list). A frame is a `?`-marked question, an optional input line (the model picker's chips,
then a query that filters as it is typed), a window of one-line rows, a status line, and a
dim `·`-separated key hint. The selected row has a `❯` that colours it rather than a
highlight bar across it; columns are as wide as the widest entry in the window, up to a
per-column cap, with `…` marking a cut; trailing columns are dim; when the width runs short
whole droppable columns go (the later ones first) before the first column is cut. The keys
every list handles are Up/Down, `C-p`/`C-n`, and query editing; what Enter, Tab and Esc do
is the page's. `efference setup`'s prompts are a separate, hand-rolled crossterm prompter
and do not use these yet. What differs between the form's own containers is the surface:
inline paints nothing and the terminal's background shows through, while the embedded modal
paints and borders because a transcript is underneath it.

An inline viewport's height is fixed at creation — ratatui's `resize` reads its argument as
the *terminal* size for `Viewport::Inline` — so a prompt reports the tallest page it can
reach (the form's `viewport_height`; its model picker's root needs room for five recents and
every connection, bounded by the connections this host actually offers rather than a
constant picked for the worst case; the session picker's is its longer page, bounded by the
terminal). Two prompts in a row each open and close their own viewport, and closing erases
from the origin down, so the second starts where the first began. Rows pack to the top so
the slack falls below the key hint rather than between the fields and it.

`session_form.rs` owns the draft, focus, and page transitions; `draft.rs` is the override
state and its conversion to the request; `pages.rs` is where the editors commit; `picker.rs`,
`picker_rows.rs`, and `matcher.rs` are the model picker's keys, rows, and matching rule, and
`picker_render.rs` gives it its columns and chips for `inline_list.rs`; `render.rs` renders into a supplied rectangle with supplied styles; `inline.rs` is the wired and the offline entry points (the viewport lifecycle is `tui/inline.rs`).
`sessions.rs`, `keyboard.rs`, and `render_sessions.rs` are the embedded integration. The form receives Escape and Ctrl-C before the session's handlers and
blocks mouse events from reaching the transcript; cancelling returns to Sessions without
touching the attached loop.

With the `dev` feature, `efference dev session-form` draws the same form in an inline
viewport with no host behind it; `--embedded` opens it inside the offline gallery. Neither
contacts a host, and the form says so — "no host is resolving this draft" is a different
state from a preview in flight, and an all-`—` column means the first one when nothing was
ever sent.
