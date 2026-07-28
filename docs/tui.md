# The TUI client

`efference tui` is the human-facing client. It is just-another-socket-client: it subscribes to the daemon's SSE streams over the same [control channel](control-channel.md) that `attach` uses, and it drives turns and approvals through the same HTTP verbs. The renderer is vendored-and-adapted from ferritin's interactive renderer into `src/tui/`. Ferritin already had the hard parts — a general-purpose document IR, a markdown→IR parser, syntect theming, a custom scrollbar, line wrapping, and collapsible nodes — and its render pipeline was domain-free (it renders an IR, not rustdoc), so the lift was strip-the-shell, not rewrite. Improvements flow back cleanly because the IR stays byte-identical to ferritin.

`efference gallery` (`tui/gallery.rs`) launches the real TUI on a fixed "one of each element" transcript with no daemon — a live design page that surfaces rendering gaps (literal `~~strike~~`, ordered-lists-as-bullets) the moment you look.


## The transcript is another `project(log)`

Just as the wire is a deterministic projection of the log into an Anthropic `messages` array (see [log-and-projection.md](log-and-projection.md)), `tui/projection.rs`'s

```rust
project(&[LogEntry], &LiveTail, &ViewState, pending_call: Option<&str>) -> Document<'static>
```

is a *second* projection of the same log to a different target: a renderable tree instead of a model-facing message list. It reuses the wire projection's rules where they apply — distillate-else-raw for tool results, the streamed-vs-committed dedup (where the wire uses `streamed_*` flags, the TUI projection uses the structural `LiveTail`) — and diverges where a human reader differs from the model: nothing is elided or coalesced (the human sees the whole record), and the projection carries a deliberate **information architecture** rather than a flat dump of labeled blocks.


## Information architecture: conversational and minimalist

The projection emits a sequence of `Turn { author, blocks }`. Authorship is **structural, not lexical**: the renderer paints a colored accent gutter for the user's turn and leaves the assistant unmarked as the default voice, so you *see* who's speaking instead of reading a `▌ you` / `▌ assistant` noun.

Each tool call is **paired with its result** into one collapsible *exchange* headed by the call:

```
❯ bash · echo hi
line one
line two
```

The result has no `result #N` / `[seq]` label — it is obviously the response to the call directly above it. The leading glyph unifies state and authorship: `❯` at rest, `✗` (red) when the result errored, `▶`/`▼` when the result is long enough to collapse (collapse is a toggle — peek then re-close, keyed by `call_id`). When a call is awaiting approval, the glyph is `⏸` (accent color).

Because all tools are known at compile time, results get **per-tool representations** instead of a raw JSON/text dump:

- `bash` — rendered as a shell code block; a single-line command goes inline on the exchange header, a multi-line command overflows into the body.
- `read` — strips the sentinel framing (`==<token> BEGIN <path>, LENGTH: n` … `END`) and highlights the file body by extension.
- `edit` / `undo_edit` / `retarget_edit` — parses the `===DIFF===` section into a color-coded `DocumentNode::Diff` (added/removed/hunk/context lines).
- Unrecognized tools — falls back to a plain code block of JSON input.

The throughline: lean on pre-attentive cues (color, gutter, glyph) and tool-specific shape, not repeated labels.


## View state: surviving re-projection

The transcript re-projects on every streamed token, and any exchange can be re-projected at any time (a result or verdict arriving rebuilds the entry that made the call). That means a `DocumentNode`'s collapse level and raw-vs-wire toggle cannot live inside the rebuilt `Document` — they would reset mid-stream. They live instead in `tui::state` as a `projection::ViewState` keyed by `call_id`, and the projection reads that state when building each exchange. `ViewState` also records which `call_id`s were toggled, since a view flip is not a log event and the incremental projection has no other way to learn of it.

```rust
struct ViewState {
    expanded: HashSet<String>,   // call_ids the human expanded to Full
    raw:      HashSet<String>,   // call_ids flipped to raw output
}
```

The `r` key flips a focused tool result between its distilled wire form and its full raw output. Both are already client-side from the log entry's `WireState` and `RawResult` (see [the redaction fork](redaction-fork.md)), so this is a pure render choice with no round-trip. When showing raw over a distillate, the exchange header gains a `· raw` annotation so the human knows they are off the default view.


## The live tail

In-flight streaming deltas not yet committed to the log live in a `LiveTail`:

```rust
struct LiveTail {
    assistant: String,            // in-flight prose this turn
    tools: Vec<(String, String)>, // (call_id, accumulated output)
    phase: Option<Phase>,         // Thinking { text } | CallingTool { name, bytes }
}
```

**Thinking is kept, not just streamed.** A committed turn's `thinking` blocks render as a keyed collapsible at the head of the turn, in the same dimmed register as the live phase — thinking is never the answer and must not read as it. Collapsed is the default, but a collapsed block still shows its opening paragraphs with a `(+N)` hint, so the gist is always readable and the whole of it is one keypress away. That asymmetry is the point: reasoning streams past far too fast to read at generation speed, so the live phase is only a "something is happening" signal and *this* is the copy a human actually reads. The block is keyed by the entry's `seq` (thinking has no `call_id`) so its expand state survives the re-projection every streamed token triggers. Encrypted (`RedactedThinking`) blocks render a line saying so rather than nothing, so the record never implies the model didn't think when the provider merely withheld the text.

A note on the accordion this exposed: `clip_node` can only shrink a single code/diff node, so a *multi-node* body (thinking's paragraphs) used to render in full at either level — "collapsed" differed from "expanded" only by the triangle. Collapsed bodies now stop after `BRIEF_LINES` worth of children, which is a no-op for the single-code-block bodies an exchange builds.

`project` renders the committed log first, then appends the live tail — in-flight assistant prose as its own turn, each running tool as a result-less exchange. When a committed `LogEntry` arrives, `LiveTail::commit` clears the superseded buffer (assistant prose on `Payload::Assistant`, tool output on `Payload::ToolResult`), mirroring the wire projection's `streamed_*` dedup flags, now structural.

**The live phase: a turn that produces no prose is still visibly working.** The streaming sink is typed (`api::Streamed` — `Text` / `Thinking` / `ToolStart` / `ToolInput`), so thinking and a tool call's arriving input reach the client as their own `/stream` events (`thinking_delta`, `tool_call_start`, `tool_call_input`) rather than not at all. They fold into `phase`, rendered in the live tail's dimmed register beneath the in-flight prose: `✻ thinking… <the thinking so far>`, or `❯ calling <tool>…` over the JSON input as it accumulates. Three rules:

- **In full, never truncated.** The point is to watch the work as it happens, so the phase shows everything that has arrived — thinking as dimmed prose, the input as dimmed wrapped lines. (Dimmed lines rather than a `code_block` because the boxed render doesn't wrap content to its inner width, so a long input would be clipped at the border — the open box-wrap defect in [backlog/dogfooding-friction.md](../backlog/dogfooding-friction.md).)
- **Shown, never parsed.** A tool call's input is a partial slice of a JSON document mid-stream, so it renders as raw text, which claims no structure. Rendering it *structurally* (`jiter`'s partial mode, `actson`'s push parser) is a real option and its own design question ([backlog/tui-streaming-lifecycle.md](../backlog/tui-streaming-lifecycle.md)); the constraint to keep is that the display must not assert structure the arrived bytes don't support.
- **Prose supersedes it**, one phase supersedes another, and a committed turn clears it: prose is its own evidence of progress, acting supersedes thinking, and the committed entry is the record — which the transcript now keeps (below).

Without this the window between submitting a turn and the first prose token was blank, and blank is indistinguishable from a hung harness — a risk of interrupting or abandoning a live session on false signal. The daemon's own log has the matching fix: it prints `(generating — turn N)` before blocking on the model, so its last line is no longer a stale `idle`, which reads as *finished*.


## Pending user turns: the delivery indicator

A submitted turn is not lost while a trajectory runs. `submit_composer` pushes the text onto an ordered `pending_turns` queue and reprojects at once; `project` renders each as a **dimmed, `⋯`-marked user turn** after the live tail — the optimistic "handed to the daemon, not yet on the wire" echo. When the matching `User` `LogEntry` streams back via `/events` (that entry *is* `project(log)`, i.e. now on the wire), `handle_event` drops it from the queue by content match, and it re-renders as a normal committed turn. The pending→committed flip is a faithful delivery signal, not cosmetic: it mirrors, on the operator's side, the daemon's mid-trajectory seam-drain (backlog/participation/README.md, [control channel](control-channel.md)).


## The pace indicator

The operator's pace (backlog/participation/README.md) is polled from `GET /state`'s `pace` into a `PaceView` and surfaced **loud** in the status bar: a `⏸ HELD` / `⏱ 10s` indicator ahead of the status text whenever the pace is non-default (the default fast-and-unheld state adds no chrome). It is status-bar chrome, never transcript content — pacing never reaches the log or the wire. The transcript keys `+`/`-` step the speed dial along a coarse ladder and `p` toggles the hold, each optimistically updating the local `PaceView` on the keypress (the poll reconciles) and issuing `POST /pace` over a dedicated pace channel. Because a slow pace *is* the window during which the operator has room to read and type, pacing and the mid-trajectory injection compose: the sleep is exactly when a message can be interjected.


## Graduated Esc

`Esc` is a **state machine, not a press-count** (backlog/participation/README.md): each press's effect is a pure function of the current state — so there is no counter, no timing debounce, and nothing to reset. Precedence, gentlest first (`handle_escape` in `tui/state.rs`):

0. **an inline sub-state is open** (the pace [widget](#the-control-strip), or a [stager](#the-control-strip) editing an argument) → revert/cancel it, restoring the value or draft at entry. These previews were never committed, so this only undoes local state.
1. **not at rest in the composer** (an overlay is open, or focus is in the transcript) → return to the composer (`Esc` is one-directional toward the composer).
2. **at the composer, held** → **interrupt** the in-flight tool (`POST /interrupt` — the loop stops at the next juncture and the running call's output is withheld from the model, plus, on a foreground bash, its process group is killed; see [control-channel.md](control-channel.md). Mid-tool is the one point pacing can't reach).
3. **at the composer, not held, loop active** → **pause** (set the `held` overlay via `POST /pace {held:true}`; the loop stops at the next between-step juncture).
4. **at the composer, not held, nothing happening** → **swallow** (a silent no-op).

So sitting in the composer while the agent works, *one* `Esc` pauses and a second interrupts — no wasted "return to composer" press; and jamming `Esc` from anywhere still walks return → pause → interrupt. "Loop active" is read from `/state`'s **`running`** flag (unioned with the streaming `pending_request`), which — unlike streaming deltas — stays true through a *silent* long-running tool, so `Esc` can still reach a runaway command. `Esc` only ever *enters* the hold or *fires* the interrupt; resume is the pace layer's job (send a message — submit always resumes — or the `p` key). The hold and interrupt verbs are exactly unit 2's `/pace` and the long-standing `/interrupt`, so this is a TUI-only key binding over existing daemon affordances.

Alongside kill, transcript-navigation `b` fires the **foreground→background handoff** (`POST /background`, [supervisor.md](supervisor.md)): the in-flight tool keeps running as a `bg#<n>` process instead of being killed or waited out. When the loop is idle the key is a status-line no-op (nothing sent). There is no separate note-capture flow — a message typed after `b` rides the seam-drain to the model alongside the handoff result. Both intents share one channel to the tokio side (`frontend::Interrupt`: `Kill` → `/interrupt`, `Detach` → `/background`).

> **Discoverability.** Pause is reachable where the operator lives — `Esc` in the composer — and the [control strip](#the-control-strip) now shows the pace, handoff, and kill verbs as visible, focusable items one `↑` from the composer. The per-entry expand/raw verbs are on the [block menu](#the-block-menu). The single-letter transcript verb binds (`p`/`+`/`-`/`b`/`r`/`t`) have been purged (backlog/tui-control-strip/): the verbs now answer only on the strip (as scoped mnemonics or clicks) and the block menu, and a plain letter in the transcript no-ops.


## Wiring: two threads, one crossbeam channel

crossterm's event read blocks, so the UI runs on a dedicated OS thread (`tui::ui_loop`). A tokio task (`tui::bridge`) subscribes to `/events` (durable `LogEntry` entries) and `/stream` (ephemeral `assistant_delta` / `tool_output` deltas) over the [transport](transport.md), decodes them into `TuiEvent`s, and forwards them over a crossbeam channel into the UI thread.

The UI loop drains arriving events through `InteractiveState::handle_event` and sets a `reproject_pending` flag rather than re-projecting per event. Before each paint it calls `flush_reproject`, which brings the transcript up to date once and clears the flag. A reattach replay that delivers N entries therefore costs O(N) event folds and O(1) projections, not O(N²).


## What a frame costs

A streaming session repaints ~33 times a second against a document that only grows, so the property to hold is that **a frame costs the viewport, not the log**. Three mechanisms, each with a measured reason:

**The transcript is a materialized view, not a rebuild** (`tui/projection/incremental.rs`). `project` — the from-scratch fold above — stays the reference definition and the thing tests compare against, but `Transcript::update` keeps the document between repaints and replaces only what changed. It rests on `entry_nodes` being independent per entry: a committed entry's nodes are a function of that entry, the annotations indexed against the calls it makes, and the human's per-call view toggles — never of its neighbours. So the invalidation rule is one sentence:

> A committed entry is re-projected only when a call it owns changes.

Everything that can change about an already-committed entry arrives keyed by `call_id` — a tool result, a spotter verdict, the gate moving, an expand or raw toggle — so each marks one owner dirty (`ViewState` records its own toggles for this, since a view flip is not a log event). The uncommitted tail is rebuilt every time; it is small and changes constantly. A payload that annotates a call in a new way joins the list; `invalidate_all` is the always-correct fallback.

**Layout is cached per top-level node** (`DocumentState::layouts`, keyed by render width). A node is laid out when it is *visible* or when its cached layout is missing; otherwise its height advances the cursor and its recorded actions are replayed. The cache lives beside the nodes and moves with them — `splice_nodes` is the only way the document changes, so a replaced node can never keep a stale height. The nodes above the viewport still have to be *walked* (their heights are what says where the viewport starts) but that walk is integer arithmetic, not re-wrapping. Off-screen rows already skipped their cell writes and their syntax highlighting; what they were still paying was span wrapping, and this is what stops that.

**Measure, then paint — once.** `render_frame` runs the walk in `measuring` mode first (`row_visible` is false for every row, so nothing is written and no highlighting runs), which fills in any missing heights and yields the document height. A stream-following viewport is then pinned to the new bottom *before* the paint, so there is no stale-offset frame to correct with a second full draw.

`document.laid_out` — how many nodes the last walk actually laid out — is what the cost tests in `harness/test/render_cost.rs` assert on: an 8× longer log must not lay out more nodes for one streamed token. That is the regression guard, in place of a timing test.


## The composer

`tui/composer.rs` is a dependency-free, emacs-keybind multiline editor. Its interface is deliberately swappable: `handle_key` / `render` / `take` (or `clear`). The buffer is `Vec<Vec<char>>` — one row per line — so cursor columns are plain char indices, correct across UTF-8 without byte-boundary juggling.

`tui-textarea` was the intended first cut but is pinned to an older ratatui/unicode-width ecosystem and does not resolve against ratatui 0.30, hence the hand-rolled editor. The narrow interface makes replacing it later surgical.

**Focus model.** Focus lives on one vertical axis — transcript, [control strip](#the-control-strip), composer — and the same physical key takes a context-dependent meaning (`C-p` is "previous line" composing, "previous unit" navigating), the way an editor's motion keys don't stop existing when you leave the buffer. There is no modal toggle: you cross between contexts by *moving*. `↑` at the top of the composer (the `0,0` detent: one press lands at the start of the first line, the next crosses the edge) spills focus onto the control strip, and `↑` again into the transcript; `↓` reverses — past the last unit it stops at the strip, then falls back into the composer; `Esc` is a graduated *return to steady state* (see [Graduated Esc](#graduated-esc) below) whose first rung returns to the composer from any stop. In the transcript context every block is a focus **unit** — each prose turn and each exchange — and the focused unit is marked by an accent glyph **`▌`** down its full height (a glyph, not reversed-video, so it shows on any theme and in the snapshot grid). `↑/↓` (or `C-p`/`C-n`) move between units; `←`/`→` choose among the focused unit's [block-menu](#the-block-menu) verbs and `Enter` fires the selected one (the collapse toggle by default). The verb keys — `r` raw, `t` theme, `+`/`-` pace, `p` pause, `b` background — are **not** bound here: after the bind purge (backlog/tui-control-strip/) each lives where it is shown (raw on the block menu, the rest on the strip), so a plain letter in the transcript is a clean no-op. The surviving letter binds are navigation and utility only (`g`/`G` top/bottom, `m` mouse, `?`/`h` help, `q` quit); typing never leaks into the composer. Submit in the composer drives `POST /turn`; submit while a gate is pending resolves the gate (below); `C-d` on an empty composer quits (shell EOF).


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

`kill` and `interrupt` are **one verb** (`StripVerb::Interrupt`, `POST /interrupt`) under two labels, because it keeps two promises and only one of them is universal. Everywhere, it stops the loop at the next juncture and withholds the in-flight call's output from the model ([control-channel.md](control-channel.md)). On a foreground `bash` it *additionally* kills the process group — the stronger promise, and the one worth naming, so there it reads `kill`. Elsewhere — an MCP tool mid-call, or the model still generating — it reads `interrupt`. It is shown in every active state: before the latch it would have been a button that did nothing outside bash (nothing awaits the abort future during a synchronous MCP call), which is why the strip previously offered no interruption verb there at all.

Two transient sub-states own the row outright and carry no tail: staging (no items at all — the row is the staging note) and the active pace stepper (only the loud `▴ pace <v> ▾` and its note). Both are described under item classes below.

Because the item list is a pure function of polled state plus the committed log, re-ranking happens only at state transitions — never while the strip sits stable.

**Which tool is running** comes from the log the TUI already holds, with no daemon round-trip: the loop appends the assistant entry *before* running its call and the `tool_result` after (`src/agent.rs`), so the last call with no paired result is what is executing now (`in_flight_call` in `tui/state.rs`). `disable_parallel_tool_use` means at most one call per turn, so that tail is the whole answer.

The focused item fills a reserved marker cell with `▸` (a space when unfocused), so focus movement never shifts text and stays visible in the character-grid snapshots; the raised-surface color change is what pixels show, asserted in tests via `cell()`. `←`/`→` (or `C-b`/`C-f`) move between items, `⏎`/space or a mouse click activates — clicks resolve against per-frame recorded column ranges (`strip_hits`). Activation dispatches onto the same mechanisms the keybindings use (the pace and interrupt channels, gate approval); `detach` raises a quit flag the UI loop checks alongside the key handler's return, since clicks have no quit return path. A state's dim note has its width reserved out of the item budget before any item is placed — an instruction is never what the cut drops — and is then drawn adjacent to the last item that fit. The strip is chrome: never projected, never in the log.

**Scoped mnemonics.** While the strip is focused, a plain letter that matches a *shown* item's hint fires that item (`b` → background while a foreground command runs, `t` → theme, `p` → resume while held), wherever the selection currently sits — a shortcut, not a focus move. This is the only surviving letter-form of the purged verbs (backlog/tui-control-strip/): the letters answer only where their item is on the strip, never as buried global binds, so `p` does nothing from an unheld strip (which shows no resume) and pausing a *running* loop is Esc, matching graduated Esc. Items whose hint isn't a bare letter (`⎋`, or `allow…`'s empty hint) are driven by their own key or the arrows. An unmatched key falls through to the remaining transcript keys (scroll, help, quit).

**Item classes.** Most items are *buttons* — `⏎` fires the verb. `allow…` is a *stager*: a verb that takes an argument, edited in the composer. Activating it parks the operator's draft (`Staging` in `tui/state.rs`), pre-fills the derived grant spec — the bash program for a `bash` gate, empty otherwise (bare `.allow` derives a tool grant at commit) — and swaps the prompt to the verb's chip (`allow › rm█`, drawn by `render_frame` left of an untouched composer), with a dim `draft saved · ⏎ commit · ⎋ cancel` note on the strip row. `⏎` commits the edited spec through the same `.allow` submit path the typed form uses, then restores the parked draft; `Esc` cancels and restores — the new gentlest rung of graduated Esc, checked before any return/pause/interrupt step. While staging, the gate verbs leave the strip (the composer owns the interaction — a lingering `approve ⏎` would misstate what `⏎` does, and clicking it would resolve the gate out from under the stager); a gate that resolves elsewhere (another client, a daemon-side abort) likewise cancels the staging rather than letting its commit fall through to a normal turn submit. The third class is a *widget*: a verb whose value is adjusted in place. The tail's `pace` item is one — `⏎` enters an inline stepper (`PaceWidget` in `tui/state.rs`), rendered loud as `▴ pace <v> ▾` (the `▴`/`▾` are the grid-visible "stepping" tell, the way `▸` marks focus), with a dim `↑/↓ adjust · ⏎ set · ⎋ revert` note. While active, the stepper owns the row — every other item leaves, so nothing else can be focused or reflow behind the changing value — and it alone captures `↑`/`↓` (`↑` climbs toward more between-turns delay). Each step **previews** by mutating the live pace (the status bar tracks it) without pushing to the daemon; `⏎` commits the previewed speed over the pace channel and leaves, and `Esc` reverts to the speed at entry — the same gentlest rung of graduated Esc as a stager cancel, and since previews were never sent there is nothing to unsend. It replaces the earlier `slower`/`faster` buttons, so between-turns pace is adjusted only here — and since `pace` is in the tail, it is reachable in every state.


## The block menu

The strip carries *loop verbs* (act on the daemon, matter mid-composition); the **block menu** (`tui/block_menu.rs`) carries the *entry verbs* of one transcript unit — the actions on the entry itself. It travels with `↑`/`↓` focus and renders trailing the focused unit's header line (`❯ bash · ls  ▸ expand  raw`), styled like the strip's items: a reserved marker cell (`▸` selected, a space otherwise, so moving the selection never shifts text) and the same button surface. Trailing the header rather than taking its own row means moving focus never reflows the transcript — a dedicated row would repaint on every `j`/`k`.

The verbs a unit carries are a pure function of the block (`block_menu::menu_items`), computed once and read by two callers with no drift: the renderer (which has the node in hand) and the key/mouse dispatch (which looks it up by focus). The collapse toggle ranks first, so `⏎` on a focused unit still means "expand this," as it did before the menu:

- **expand / collapse** — the `ExpandBlock` toggle, offered only when the body has more lines than a collapsed block shows (`render_node::is_collapsible`); the label names the destination state.
- **raw / distilled** — the `toggle_raw` flip (docs/redaction-fork.md), offered on a tool exchange with a result. The new home for the forgettable `r`.

`←`/`→` (and `C-b`/`C-f`) choose among them, clamped at both ends; `⏎`/space or a click fires the selected one — clicks resolve against per-frame document-coordinate rects (`block_menu_hits`) *ahead of* the whole-header expand action underneath, or every menu click would just expand. The selection resets to the leading verb whenever focus moves to another unit, so `⏎` after a move is always the most relevant thing. A block with no durable `key` (a live tool's in-flight output) and a prose `Turn` (no per-entry verbs yet) carry no menu, and `⏎` there stays the no-op / plain-expand it was. The gated exchange's verbs are the gate's — they live on the strip — so it carries no menu either. Later entry verbs (full-screen, open-in-editor, actions on a backgrounded process's entry) attach here as their mechanisms land.


## Inline approval gates

The approval gate is ephemeral control state: the daemon gates *after* appending the `tool_use` to the log and *before* running the tool, but there is no SSE event for this transition. A poll task watches `GET /state`'s `pending_approval` field and emits a `TuiEvent::Gate` when it appears or resolves.

The gate renders **inline at the pending call**, not as a bottom banner. Because the loop appends the assistant `tool_use` before it gates and one tool call runs per turn, the pending call is always the tail result-less exchange. `project` receives `pending_call` + `pending_reason` and emits the gate beside that exchange, where the result will land — the spotter's reason (when it gated via the spotter — read from the `Spot` audit entry, see [permissions](permissions.md)), then the affordance carrying the `⏸`:

```
❯ bash · rm -rf /tmp/scratch
spotter gated · deletes outside the git tree — not reversible
⏸ awaiting your approval  ·  ⏎ approve · type to redirect
```

Reading order is call → what the spotter made of it → what to do, so the affordance lands adjacent to the strip carrying the verbs.

**Every judged call carries its verdict, not only a held one.** The `Spot` entries are indexed by `call_id` alongside the results and emitted with the exchange they judge, so a *cleared* call shows its reason too — which is most of them, since clearing is the common verdict and nothing ever interrupts for one. The stance leads the line (`spotter cleared · …`, `watching`, `gated`) because it is the scannable part; without it every verdict looks alike. The whole line sits in the dim register: before the decision it is why the call is held, and after it is a gloss on what the call was taken to be, so it never competes with the call or its result for attention. A resolved gate keeps its verdict, which is the only surviving record when a human approves something the spotter wanted to stop. `Origin` is not drawn — a fail-closed verdict's own reason reads "spotter unavailable", so a glyph would only repeat the line. The committed entry is the source; `pending_reason` from `/state` covers only the window where a gate has registered but its log entry hasn't reached the client, so the two never both render (that path has no stance, and reads `spotter · …`).

Whether this much per-call annotation earns its space is an open question the dogfooding loop is meant to answer; a suppression setting is the obvious lever if it turns out to be noise.

**Structured annotations are siblings of the exchange, never children of it.** The exchange's collapsible body holds only what the agent produced — the call's arguments and its result — so the body's truncation can never reach something the harness added *about* the call. This is not a stylistic split: while the reason and the affordance were nested, a multi-line command filled the collapsed body on its own and clipped both, and the `⏸` (then the exchange's resting glyph) was replaced by the disclosure triangle the renderer substitutes once a body is collapsible. A gate on any realistic command therefore showed no reason, no affordance, and no sign the call was held. Anything that can be collapsed away is not a marker, so the `⏸` rides the affordance instead.

The same split is where per-call structure goes as it lands; the eventual shape is a typed exchange owning its structured sub-items rather than a flat sibling list.

The status bar mirrors this affordance for the scrolled-away case, and the [control strip](#the-control-strip) simultaneously carries the full verb set (`approve ⏎ · allow… · abort`), so `↑ ⏎` approves without typing and the strip — not instruction text — is the legend for the richer verbs. Strip `approve`/`abort` resolve the gate directly *without* clearing a drafted redirect the operator decided against sending (unlike an empty-composer `⏎`, which routes through the submit path below).

Submitting from the composer while a gate is pending resolves it with the same three-valued `Approval` the loop consumes (from `src/frontend.rs`):

- empty → `Approval::Approve`
- `.abort` → `Approval::Abort`
- `.allow [prefix]` → `Approval::ConfigEdit` — add a rule, then run the call iff it now clears (the mechanism re-checks; nothing rides along unchecked, see [permissions](permissions.md)). An explicit prefix grants that bash command-prefix; bare `.allow` derives the grant from the gated call (the bash program, or a structured tool's name). Written to the session's `config.toml`, like every committed grant.
- any other text → `Approval::Redirect(text)`

The `.abort` / `.allow` sentinels are retained as the submit *mechanism* — the strip's `allow…` stager commits through the same `.allow` path, so grant semantics live in one place — they're just no longer advertised as things to type.

Resolving replaces the inline affordance with the real result (or, on redirect/abort, an error result), and the call stays in the transcript with that outcome in place of output.

## The sidebar

When the terminal is wide enough (≥ a threshold), a **right-docked sidebar** carries the glanceable, self-contained widgets that don't belong in the transcript: `PROCESSES` (the live background processes, off the old bottom strip), `STATS` (token/cache telemetry, off the status bar), and `POLICY`. It is chrome, fed by the `/state` poll (`tui/render_sidebar.rs`), with a `│` dividing it from the transcript; the composer spans the transcript column only, and below the dock threshold the sidebar is hidden (a narrow-mode drawer is future work). Its `Rect` is remembered so `Alt-S` can screenshot just those columns.

`POLICY` renders the **review level** (`/state`'s `review_level` — who reviews a gated call, read-only here; set it with `efference review-level`, the interactive gear selector deferred to the ergonomics pass) followed by the effective perimeter — scope, commands, paths, tools — read from `/state`'s `role`. Pending **proposals** (from `/state`'s `proposals`: prefix-promotion, a session's config-write) render *in the same section, in their committed sort position*, flagged with a trailing `⊕` in a muted style — a proposal is a projection of its committed self, so committing it (over `POST /proposals/:seq`) drops the `⊕` in place without reflow. This is the human/orchestrator surface for the config-edit loop (see [permissions](permissions.md)).


## Iterating the display without being blind

`tui/harness.rs`'s `Screenplay` (test-only) scripts a transcript through the *real* event and input handlers, renders to a ratatui `TestBackend`, and frames the buffer as a ruled character grid pinned by an `insta` snapshot — so a wrap or layout change is a legible diff. The grid drops color by design so that `cell(x, y)` / `find(needle)` assertions can check foreground styles and modifiers (diff green/red, error red, the user's blue accent gutter) that a snapshot alone cannot show. Keys route through the full `handle_key_event` (`Screenplay::route_key`), so focus transitions — the spill, Esc-home, unit navigation — are exercised as production code, not a synthetic shortcut.

Two facilities close the gap between *what a snapshot shows* and *what is actually reachable*. The grid encoder is one shared module (`tui/grid.rs`: `frame_grid`, and `frame_grid_region` for a sub-rectangle); a **dev screenshot keybind** (gated by `EFFERENCE_DEV`) runs it against the *live* `InteractiveState` and writes to `.efference/screenshots/` — `Ctrl-S` captures the full screen, `Alt-S` just the sidebar columns (a narrow capture of a wide terminal) — so a live capture is byte-comparable to a committed `.snap`. And `Screenplay::replay` feeds a real recorded log (`persist::load` of an `.efference/*.jsonl`) through the same handler, so a snapshot taken at index *n* is a state the daemon actually produced — reachable by construction, not hand-authored. A trimmed real session lives in `tui/fixtures/`.


## Lifetimes

The IR keeps ferritin's `Cow<'a>` borrowing, used as `Document<'static>` over owned strings. Nodes are replaced wholesale rather than edited in place — the incremental projection splices a rebuilt entry's nodes over its old ones — so the borrowing costs nothing here, and staying byte-identical to ferritin keeps improvements backportable both ways.


## Backlog

Remaining polish — the box→`│` connector refinement for result bodies, focus *into* the sidebar (x/y/z navigation, committing a proposal in place), the narrow-mode scrimmed drawer, the at-rest unit boundary, refreshing the help-screen keymap text, and gates-as-synthetic-`/events` to drop the poll task — is tracked in the backlog.
