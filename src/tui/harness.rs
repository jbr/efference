//! "Playwright for TUIs" — a test harness for snapshotting the interactive TUI
//! as a plain-text character grid (no colors, no pixels).
//!
//! The render path ([`InteractiveState::render_frame`](super::state::InteractiveState))
//! is already a pure function of state → ratatui `Buffer`. This harness wraps
//! that in the three layers iterating-a-TUI-blind needs:
//!
//! 1. **Scenario** — [`Screenplay::new`] builds an `InteractiveState` at a chosen
//!    width×height with no daemon/network behind it.
//! 2. **Drive** — [`Screenplay::type_str`] / [`Screenplay::key`] /
//!    [`Screenplay::event`] push through the *real* input + event handlers, so a
//!    snapshot reflects production code, not a synthetic shortcut.
//! 3. **Serialize** — [`Screenplay::render`] draws to a ratatui `TestBackend` and
//!    frames the buffer with a column ruler + row gutter + border, so wrap/layout
//!    bugs are *visible* in the diff (you can see exactly where column N falls).
//!    [`Screenplay::snapshot`] hands that to `insta` as a committed `.snap`.

use crossbeam_channel::{Sender, unbounded};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Modifier},
};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use unicode_width::UnicodeWidthStr;

use super::TuiEvent;
use super::render_context::{OutputMode, RenderContext};
use super::state::InteractiveState;
use super::theme::InteractiveTheme;
use crate::frontend::Approval;
use crate::log::{
    CallId, ContentBlock, LogEntry, Payload, RawResult, Timestamp, ToolCall, WireState,
};

/// A scripted TUI session: build state, drive input/events, snapshot the screen.
///
/// The lifetime is the projected document's; the no-arg constructor starts with
/// an empty document (`'static`), which is all the composer scenarios need. The
/// channel far-ends are held so sends from inside the state never error.
pub(crate) struct Screenplay<'a> {
    state: InteractiveState<'a>,
    width: u16,
    height: u16,
    next_seq: u64,
    _event_tx: Sender<TuiEvent>,
    _submit_rx: UnboundedReceiver<String>,
    _approve_rx: UnboundedReceiver<(String, Approval)>,
}

impl Screenplay<'static> {
    /// A fresh session at the given terminal size, empty transcript, default theme.
    pub(crate) fn new(width: u16, height: u16) -> Self {
        // Load the same theme the real app defaults to, so the harness sees the
        // production color scheme (`RenderContext::new` alone leaves an empty
        // `ColorScheme` where every semantic style falls back to one gray). Snapshots
        // drop color, so this only matters to `cell`-based style assertions — but it
        // makes those faithful to what the user actually sees.
        let rc = RenderContext::new()
            .with_interactive(true)
            .with_output_mode(OutputMode::Tty)
            .with_terminal_width(width as usize)
            .with_theme_name(super::DEFAULT_THEME)
            .expect("the default theme loads");
        let theme = InteractiveTheme::from_render_context(&rc);
        let (event_tx, event_rx) = unbounded();
        let (submit_tx, submit_rx) = unbounded_channel();
        let (approve_tx, approve_rx) = unbounded_channel();
        let state = InteractiveState::new(event_rx, submit_tx, approve_tx, rc, theme);
        Self {
            state,
            width,
            height,
            next_seq: 1,
            _event_tx: event_tx,
            _submit_rx: submit_rx,
            _approve_rx: approve_rx,
        }
    }
}

impl Screenplay<'_> {
    /// Type literal text into the composer, one key at a time, through the real
    /// `Composer::handle_key`. `\n` inserts a newline (Alt-Enter), matching the
    /// composer's multiline key rather than submitting.
    pub(crate) fn type_str(&mut self, text: &str) -> &mut Self {
        for ch in text.chars() {
            let key = if ch == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
            } else {
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
            };
            self.state.composer.handle_key(key);
        }
        self
    }

    /// Send one key to the composer (e.g. `KeyCode::Backspace`, `Ctrl-A`).
    pub(crate) fn key(&mut self, code: KeyCode, mods: KeyModifiers) -> &mut Self {
        self.state.composer.handle_key(KeyEvent::new(code, mods));
        self
    }

    /// Feed a [`TuiEvent`] through the real handler — committed entries, streaming
    /// deltas, gate changes — exactly as the daemon bridge would.
    pub(crate) fn event(&mut self, event: TuiEvent) -> &mut Self {
        self.state.handle_event(event);
        self
    }

    /// Commit a [`Payload`] to the transcript as the next sequential log entry,
    /// exactly as the durable `/events` tail would deliver it. `seq` auto-increments
    /// so scenarios read as a script rather than bookkeeping.
    pub(crate) fn commit(&mut self, payload: Payload) -> &mut Self {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.event(TuiEvent::Entry(LogEntry {
            seq,
            turn: 1,
            at: Timestamp(0),
            payload,
        }))
    }

    /// A committed user turn (markdown prose).
    pub(crate) fn user(&mut self, text: &str) -> &mut Self {
        self.commit(Payload::User(vec![ContentBlock::Text(text.into())]))
    }

    /// A committed assistant turn: markdown prose plus zero or more tool calls.
    pub(crate) fn assistant(&mut self, text: &str, calls: Vec<ToolCall>) -> &mut Self {
        self.commit(Payload::Assistant {
            text: vec![ContentBlock::Text(text.into())],
            calls,
        })
    }

    /// Expand every collapsed exchange in the transcript, driving the real
    /// [`ExpandBlock`](crate::tui::document::TuiAction::ExpandBlock) action the
    /// `⏎`/`space`/click handlers use. This is how a scenario shows the *expanded*
    /// result form rather than the default collapsed snippet. Recurses into
    /// [`Turn`](crate::tui::document::DocumentNode::Turn)s, where exchanges live.
    pub(crate) fn expand_all(&mut self) -> &mut Self {
        use crate::tui::document::{DocumentNode, NodePath, TuiAction};

        // Collect the paths of all TruncatedBlocks (exchanges nest one level deep
        // inside a Turn), then apply each expansion.
        fn collect(nodes: &[DocumentNode<'_>], prefix: NodePath, out: &mut Vec<NodePath>) {
            for (i, node) in nodes.iter().enumerate() {
                let mut path = prefix;
                path.push(i);
                match node {
                    DocumentNode::TruncatedBlock { .. } => out.push(path),
                    DocumentNode::Turn { blocks, .. } => collect(blocks, path, out),
                    _ => {}
                }
            }
        }
        // Events only flag a pending reproject; realise it before inspecting the
        // document so the collapsed blocks we expand actually exist.
        self.state.flush_reproject();
        let mut paths = Vec::new();
        collect(&self.state.document.document.nodes, NodePath::new(), &mut paths);
        for path in paths {
            super::events::handle_action(
                &mut self.state.document.document,
                &mut self.state.view,
                TuiAction::ExpandBlock(path),
            );
        }
        self.state.viewport.cached_layout = None;
        self
    }

    /// Flip a tool-result exchange to its raw output (the `r` key, DESIGN §10.5) —
    /// the view-state toggle that survives re-projection, here driven directly by
    /// `call_id` rather than through keyboard focus.
    pub(crate) fn show_raw(&mut self, call_id: &str) -> &mut Self {
        self.state.view.toggle_raw(call_id);
        self.state.reproject();
        self
    }

    /// A committed tool result for `call_id`, projected at its `wire` state.
    pub(crate) fn tool_result(
        &mut self,
        call_id: &str,
        content: &str,
        is_error: bool,
        wire: WireState,
    ) -> &mut Self {
        self.commit(Payload::ToolResult {
            call_id: CallId(call_id.into()),
            raw: RawResult {
                content: content.into(),
                is_error,
            },
            wire,
        })
    }

    /// Mark the agent blocked on approval for `call_id` — the pending gate the
    /// daemon's `/state` poll surfaces, driven through the real `Gate` event so the
    /// inline gate renders exactly as in the app (DESIGN §10.5).
    pub(crate) fn pending_gate(
        &mut self,
        call_id: &str,
        name: &str,
        input: serde_json::Value,
    ) -> &mut Self {
        self.event(TuiEvent::Gate(Some(crate::tui::PendingGate {
            call_id: call_id.into(),
            name: name.into(),
            input,
        })))
    }

    /// Draw the current state to a fresh `TestBackend` and return the resulting
    /// buffer plus the terminal cursor position. Idempotent — never mutates
    /// observable state. The shared primitive behind [`render`](Self::render) /
    /// [`cell`](Self::cell) / [`find`](Self::find).
    fn draw(&mut self) -> (Buffer, Option<(u16, u16)>) {
        let mut terminal = Terminal::new(TestBackend::new(self.width, self.height)).unwrap();
        // Mirror the UI loop: events only flag a pending reproject, so realise it
        // before painting (the loop calls `flush_reproject` before every draw).
        self.state.flush_reproject();
        let state = &mut self.state;
        terminal.draw(|frame| state.render_frame(frame)).unwrap();
        // The terminal cursor is backend state, not a buffer cell — capture it so
        // the grid can mark where the caret actually is (the composer sets it via
        // `Frame::set_cursor_position` while composing).
        let cursor = terminal.get_cursor_position().ok().map(|p| (p.x, p.y));
        (terminal.backend().buffer().clone(), cursor)
    }

    /// Render the current state to a framed character grid.
    pub(crate) fn render(&mut self) -> String {
        let (buf, cursor) = self.draw();
        frame_grid(&buf, self.width, cursor)
    }

    /// The symbol **and style** of the cell at buffer coords `(x, y)` — the same
    /// column/row the snapshot's ruler and gutter label. The character grid drops
    /// color by design (see [`frame_grid`]); this is the escape hatch to *view or
    /// assert* the foreground / background / modifiers (bold, italic, crossed-out)
    /// that a `.snap` can't show — e.g. confirming a diff's `+`/`-` lines are green
    /// and red. Pair with [`find`](Self::find) to locate a position by content.
    pub(crate) fn cell(&mut self, x: u16, y: u16) -> CellStyle {
        let (buf, _) = self.draw();
        let cell = &buf[(x, y)];
        CellStyle {
            symbol: cell.symbol().to_string(),
            fg: cell.fg,
            bg: cell.bg,
            modifier: cell.modifier,
        }
    }

    /// The `(x, y)` buffer position where `needle` first appears, scanning rows
    /// top-to-bottom. Returns the column of the match's first cell, so a style
    /// assertion can target text by content instead of a brittle hardcoded column.
    pub(crate) fn find(&mut self, needle: &str) -> Option<(u16, u16)> {
        let (buf, _) = self.draw();
        for y in 0..buf.area.height {
            let mut row = String::new();
            let mut starts: Vec<(usize, u16)> = Vec::new();
            for x in 0..buf.area.width {
                starts.push((row.len(), x));
                row.push_str(buf[(x, y)].symbol());
            }
            if let Some(byte) = row.find(needle) {
                let x = starts
                    .iter()
                    .rev()
                    .find(|(off, _)| *off <= byte)
                    .map(|(_, x)| *x)
                    .unwrap_or(0);
                return Some((x, y));
            }
        }
        None
    }

    /// Render and assert against a committed `insta` file snapshot.
    pub(crate) fn snapshot(&mut self, name: &str) {
        insta::assert_snapshot!(name, self.render());
    }
}

/// The symbol and visual style of one rendered cell — what a character-grid
/// snapshot can't capture. Returned by [`Screenplay::cell`]; `Debug`-print it to
/// *view* a cell, or read its fields to *assert* on color/modifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CellStyle {
    pub symbol: String,
    pub fg: Color,
    pub bg: Color,
    pub modifier: Modifier,
}

impl CellStyle {
    /// Whether this cell carries a given modifier (e.g. `Modifier::CROSSED_OUT`,
    /// `Modifier::BOLD`, `Modifier::ITALIC`).
    pub(crate) fn has(&self, modifier: Modifier) -> bool {
        self.modifier.contains(modifier)
    }
}

/// Build a [`ToolCall`] for a scenario: a tool name, the `toolu_…` id that its
/// result will reference, and a JSON input value (`serde_json::json!({…})`).
pub(crate) fn tool_call(name: &str, call_id: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: CallId(call_id.into()),
        name: name.into(),
        input,
    }
}

/// Format a ratatui [`Buffer`] as a bordered, ruled character grid:
///
/// ```text
///      0         1
///      0123456789012345     <- column ruler (tens row shown when width > 9)
///    ┌────────────────┐
///  0 │› hello world   │     <- row gutter | content padded to width | border
///  1 │                │
///    └────────────────┘
/// ```
///
/// The ruler + border make wrap/overflow bugs legible: you can see precisely
/// which column a break lands on. Colors are intentionally dropped — only the
/// cell symbols are captured. Trailing all-blank rows are trimmed for focus.
///
/// When `cursor` is given, a caret line is drawn directly beneath its row with a
/// `^` under the exact column — non-destructive (never hides the cell under the
/// cursor) and unambiguous about where the insertion point sits.
fn frame_grid(buf: &Buffer, width: u16, cursor: Option<(u16, u16)>) -> String {
    let w = width as usize;

    let mut rows: Vec<String> = Vec::with_capacity(buf.area.height as usize);
    for y in 0..buf.area.height {
        let mut s = String::new();
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        rows.push(s.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }

    let gutter = "   "; // two-digit row index + space
    let mut out = String::new();

    // Tens ruler (only meaningful once the width crosses a tens boundary).
    if w > 9 {
        out.push_str(gutter);
        out.push(' '); // skip the left border column
        for x in 0..w {
            out.push(if x % 10 == 0 {
                char::from_digit((x / 10 % 10) as u32, 10).unwrap()
            } else {
                ' '
            });
        }
        out.push('\n');
    }
    // Units ruler.
    out.push_str(gutter);
    out.push(' ');
    for x in 0..w {
        out.push(char::from_digit((x % 10) as u32, 10).unwrap());
    }
    out.push('\n');

    // Top border.
    out.push_str(gutter);
    out.push('┌');
    out.push_str(&"─".repeat(w));
    out.push_str("┐\n");

    // Content rows, padded to the true display width so the right border aligns
    // even across wide (CJK/emoji) glyphs. A caret line follows the cursor's row.
    for (y, row) in rows.iter().enumerate() {
        out.push_str(&format!("{y:>2} │"));
        out.push_str(row);
        let used = UnicodeWidthStr::width(row.as_str());
        if used < w {
            out.push_str(&" ".repeat(w - used));
        }
        out.push_str("│\n");

        if let Some((cx, cy)) = cursor
            && cy as usize == y {
                let col = (cx as usize).min(w.saturating_sub(1));
                out.push_str("   │");
                out.push_str(&" ".repeat(col));
                out.push('^');
                out.push_str(&" ".repeat(w - col - 1));
                out.push_str("│\n");
            }
    }

    // Bottom border.
    out.push_str(gutter);
    out.push('└');
    out.push_str(&"─".repeat(w));
    out.push_str("┘\n");
    out
}

/// Baseline transcript scenarios — realistic user / assistant / tool blocks
/// rendered through the full frame, so formatting/rendering changes have a
/// visible before/after. These are the "what the user sees" fixtures; the
/// composer-focused scenarios live in `composer.rs`.
#[cfg(test)]
mod scenarios {
    use super::{Screenplay, tool_call};
    use crate::log::{Keep, WireState};
    use ratatui::style::{Color, Modifier};
    use serde_json::json;

    /// A realistic `preview_edit` result (real format from `.efference/*.jsonl`):
    /// preamble lines, then a `===DIFF===` section with ` `/`+`/`-` line prefixes.
    const EDIT_RESULT: &str = "Previewing: replace\n\
        Note: the editor applies a consistent formatting style to the entire file\n\n\
        💡 TIP: For focused changes you might try targeted replace operations\n\n\
        ===DIFF===\n\n\
        \x20    /// True when the buffer holds no non-whitespace text.\n\
        \x20    pub(super) fn is_empty(&self) -> bool {\n\
        -        self.lines.iter().all(|l| l.iter().all(|c| c.is_whitespace()))\n\
        +        self.lines\n\
        +            .iter()\n\
        +            .all(|l| l.iter().all(|c| c.is_whitespace()))\n\
        \x20    }";

    /// Builds a transcript whose last entry is a `preview_edit` result carrying
    /// [`EDIT_RESULT`], so both the snapshot and the color test share one setup.
    fn edit_scenario(w: u16, h: u16) -> Screenplay<'static> {
        let mut play = Screenplay::new(w, h);
        play.user("Wrap that long line.");
        play.assistant(
            "Here's the reformatted version.",
            vec![tool_call(
                "preview_edit",
                "toolu_08",
                json!({ "anchor": "fn is_empty", "content": "…" }),
            )],
        );
        play.tool_result("toolu_08", EDIT_RESULT, false, WireState::Raw);
        play
    }

    /// An edit tool's result renders as a diff: the `===DIFF===` preamble/tips are
    /// stripped, and the `+`/`-`/context lines keep their prefixes for alignment.
    /// (Color isn't in the grid — `diff_lines_are_colored` asserts that.)
    #[test]
    fn transcript_edit_diff() {
        edit_scenario(80, 32).snapshot("transcript_edit_diff");
    }

    /// The payoff for the cell-style tool: a `+` line is rendered green and a `-`
    /// line red — which the character grid can't show. Asserted by channel
    /// dominance so it holds across themes (the default theme's diff colors or the
    /// built-in fallback).
    #[test]
    fn diff_lines_are_colored() {
        let mut play = edit_scenario(80, 32);

        let (ax, ay) = play.find("+        self.lines").expect("added line on screen");
        let added = play.cell(ax + 1, ay); // a glyph past the '+' prefix
        let (rx, ry) = play.find("-        self.lines").expect("removed line on screen");
        let removed = play.cell(rx + 1, ry);

        let (ar, ag, ab) = rgb(added.fg);
        assert!(ag > ar && ag > ab, "added line should be green, got {added:?}");
        let (rr, rg, rb) = rgb(removed.fg);
        assert!(rr > rg && rr > rb, "removed line should be red, got {removed:?}");
    }

    /// The live background-process panel (DESIGN §6): a `Processes` event renders a
    /// persistent strip above the composer — one row per running process with handle,
    /// command, running/idle timing, and the last output line. Fed by the `/state`
    /// poll, it is chrome, not transcript (no reproject).
    #[test]
    fn background_process_panel() {
        use super::super::{BgProc, TuiEvent};
        let mut play = Screenplay::new(80, 16);
        play.user("watch the build");
        play.assistant("Started it — I'll keep working.", vec![]);
        play.event(TuiEvent::Processes(vec![
            BgProc {
                handle: "bg#1".into(),
                command: "cargo watch -x test".into(),
                tail: "Compiling efference\nrunning 12 tests\ntest result: ok. 12 passed".into(),
                running_secs: 252,
                idle_secs: 8,
            },
            BgProc {
                handle: "bg#2".into(),
                command: "npm run dev".into(),
                tail: "ready - started server on 0.0.0.0:3000".into(),
                running_secs: 9,
                idle_secs: 9,
            },
        ]));
        play.snapshot("background_process_panel");
    }

    /// A running tool streams raw process bytes into the live tail
    /// ([`projection`](super::super::projection) renders `live.tools` directly).
    /// That output carries control characters — ANSI color escapes (`\x1b[…m`),
    /// carriage returns from progress bars — which a durable, JSON-encoded log
    /// never holds, so this path is the only one that can deliver them to a cell.
    /// Ratatui's `cell_width` debug-asserts on a control char and panics; the live
    /// tail is replayed verbatim on every reattach, so the crash is deterministic.
    /// Rendering must instead be total: control chars fold to a glyph, no panic.
    #[test]
    fn live_tool_output_with_control_chars_does_not_panic() {
        use super::super::TuiEvent;
        let mut play = Screenplay::new(80, 16);
        play.user("run the build");
        play.event(TuiEvent::ToolOutput {
            call_id: "toolu_live".into(),
            chunk: "compiling \x1b[32mok\x1b[0m\rprogress: 50%\rprogress: 100%\n".into(),
        });
        // The assertion is simply that drawing the frame returns rather than panics.
        let screen = play.render();
        assert!(screen.contains("progress"), "live tool output is on screen");
        assert!(
            !screen.contains('\x1b') && !screen.contains('\r'),
            "no raw control chars survive into the rendered grid",
        );
    }

    /// A token with no break point that is wider than the whole text column (a
    /// long URL or path in a narrow terminal). The inline-wrap loop must hard-cut
    /// it to make progress; without that it wraps in place forever, incrementing
    /// `pos.y` until the `u16` overflows and the render panics.
    #[test]
    fn long_unbreakable_token_does_not_livelock() {
        let mut play = Screenplay::new(40, 20);
        let tok = "x".repeat(200);
        play.assistant(&format!("here: {tok} done"), vec![]);
        let screen = play.render();
        // It renders (no panic) and the token's tail survives the hard cuts.
        assert!(screen.contains("xxxx"), "the long token is on screen");
        assert!(screen.contains("done"), "text after the token still renders");
    }

    /// Unpack an (asserted) RGB cell color into channels.
    fn rgb(color: Color) -> (u8, u8, u8) {
        match color {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("expected an RGB diff color, got {other:?}"),
        }
    }

    /// Renders the `efference gallery` transcript (every element type) headlessly,
    /// at a tall terminal so most of it fits — a regression guard for the live
    /// design page and a single place to eyeball the whole element set at once.
    #[test]
    fn gallery() {
        use super::super::TuiEvent;
        let mut play = Screenplay::new(80, 90);
        for entry in crate::tui::gallery::log() {
            play.event(TuiEvent::Entry(entry));
        }
        play.snapshot("gallery");
    }

    /// The bread-and-butter shape: a user question, an assistant turn with
    /// markdown prose (bold, inline code, a bullet list) and a tool call, then the
    /// tool's result. This is the canonical transcript baseline.
    #[test]
    fn transcript_coding_turn() {
        let mut play = Screenplay::new(80, 40);
        play.user("Why does `project()` rebuild the whole wire array every turn? Can we cache the committed prefix instead?");
        play.assistant(
            "Good question. The rebuild is **deliberate**, not an oversight:\n\n\
             - `project` is a pure fold over the log, so it's cheap to recompute\n\
             - caching a prefix would mean tracking invalidation across `WireState` flips\n\
             - the prompt cache already covers the *wire* cost — only the prefix bytes matter\n\n\
             Let me confirm the current signature before we touch it.",
            vec![tool_call(
                "bash",
                "toolu_01",
                json!({ "command": "grep -n 'fn project' src/project.rs" }),
            )],
        );
        play.tool_result(
            "toolu_01",
            "12:pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {\n\
             88:    fn project_coalesces_tool_results() {",
            false,
            WireState::Raw,
        );
        play.assistant(
            "Right — `project(&[LogEntry]) -> Vec<ApiMessage>` is the whole surface. \
             Caching would have to wrap *that*, which breaks byte-stability. I'd leave it.",
            vec![],
        );
        play.snapshot("transcript_coding_turn");
    }

    /// A failing tool call — the error result path (struck-through label, the
    /// `error #seq` tag) so error styling has a baseline.
    #[test]
    fn transcript_error_result() {
        let mut play = Screenplay::new(80, 24);
        play.user("Run the test suite.");
        play.assistant(
            "Running the full suite now.",
            vec![tool_call(
                "bash",
                "toolu_02",
                json!({ "command": "cargo test" }),
            )],
        );
        play.tool_result(
            "toolu_02",
            "error[E0599]: no method named `reproject` found for struct `InteractiveState`\n\
             error: could not compile `efference` (lib test) due to 1 previous error",
            true,
            WireState::Raw,
        );
        play.snapshot("transcript_error_result");
    }

    /// The same coding turn, but with every collapsed block expanded — so the
    /// baseline shows how tool-call JSON input and raw tool output actually render
    /// (a fenced code block inside the outdented `╰─` border), not just the
    /// collapsed `[...]` snippet. This is the form behind every `⏎`-to-expand.
    #[test]
    fn transcript_expanded_tool_io() {
        let mut play = Screenplay::new(80, 30);
        play.user("Show me the project function signature.");
        play.assistant(
            "On it.",
            vec![tool_call(
                "bash",
                "toolu_01",
                json!({ "command": "grep -n 'fn project' src/project.rs" }),
            )],
        );
        play.tool_result(
            "toolu_01",
            "12:pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {\n\
             88:    fn project_coalesces_tool_results() {",
            false,
            WireState::Raw,
        );
        play.expand_all();
        play.snapshot("transcript_expanded_tool_io");
    }

    /// A bash call renders its `command` as a `bash` code block — true newlines and
    /// shell escapes, not a `{"command": "…\n…"}` JSON envelope. A heredoc proves
    /// the multiline payload survives as real lines.
    #[test]
    fn transcript_bash_command_block() {
        let mut play = Screenplay::new(80, 20);
        play.user("Write the version file.");
        play.assistant(
            "Writing it now.",
            vec![tool_call(
                "bash",
                "toolu_06",
                json!({ "command": "cat > VERSION <<'EOF'\n0.1.0\nbuild: nightly\nEOF" }),
            )],
        );
        play.tool_result("toolu_06", "", false, WireState::Raw);
        play.snapshot("transcript_bash_command_block");
    }

    /// GFM `~~strikethrough~~` renders as actual struck-through text (the `~~`
    /// markers gone, `Modifier::CROSSED_OUT` applied) — neither of which the
    /// character grid shows, so `cell` checks the modifier.
    #[test]
    fn markdown_strikethrough_is_crossed_out() {
        let mut play = Screenplay::new(60, 8);
        play.assistant("keep ~~gone~~ stay", vec![]);
        assert!(play.find("~~").is_none(), "the ~~ markers should be consumed");
        let (x, y) = play.find("gone").expect("strikethrough text renders");
        assert!(
            play.cell(x, y).has(Modifier::CROSSED_OUT),
            "strikethrough text should be crossed out",
        );
    }

    /// Authorship is carried by color, not a word: the user's turn gets a blue
    /// accent gutter (`┃`) in the left margin; the assistant has none. The grid
    /// shows the glyph but not its color, so `cell` checks the accent.
    #[test]
    fn user_turn_has_a_blue_accent_gutter() {
        let mut play = Screenplay::new(60, 8);
        play.user("hello there");
        let (_, y) = play.find("hello there").expect("user prompt on screen");
        let gutter = play.cell(1, y); // the reserved left-margin gutter column
        assert_eq!(gutter.symbol, "┃", "user turn carries a gutter glyph");
        let (r, _g, b) = rgb(gutter.fg);
        assert!(b > r, "the user gutter should be a blue accent, got {gutter:?}");
    }

    /// A failed exchange reads as an error via a red `✗` prompt (replacing the
    /// normal `❯`) — and explicitly NOT strikethrough (which now means deletion).
    /// The character grid can't show color; `cell` asserts it.
    #[test]
    fn error_exchange_prompt_is_red() {
        let mut play = Screenplay::new(80, 16);
        play.user("Run it.");
        play.assistant("", vec![tool_call("bash", "toolu_07", json!({ "command": "false" }))]);
        play.tool_result("toolu_07", "command failed", true, WireState::Raw);

        let (x, y) = play.find("✗").expect("error prompt is on screen");
        let style = play.cell(x, y);
        assert!(
            !style.has(Modifier::CROSSED_OUT),
            "an error is not a deletion — it shouldn't be struck through: {style:?}",
        );
        let (r, g, b) = rgb(style.fg);
        assert!(r > g && r > b, "error prompt should be red, got {style:?}");
    }

    /// The file tools render tool-specifically: `read`/`search`/`list` calls show
    /// their path/pattern as one concise line (not a JSON envelope), and a `read`
    /// result drops its `==… BEGIN …==` sentinel framing and highlights by file type.
    #[test]
    fn transcript_file_tools() {
        let mut play = Screenplay::new(80, 40);
        play.user("Open state.rs, then search and list.");
        play.assistant(
            "Reading the file:",
            vec![tool_call("read", "toolu_r1", json!({ "paths": ["src/tui/state.rs"] }))],
        );
        play.tool_result(
            "toolu_r1",
            "==MK7 BEGIN /Users/jbr/code/efference/src/tui/state.rs, LENGTH: 96 MK7==\n\
             pub struct InteractiveState<'a> {\n\
             \x20   pub document: DocumentState<'a>,\n\
             \x20   pub viewport: ViewportState,\n\
             }\n\
             ==MK7 END /Users/jbr/code/efference/src/tui/state.rs MK7==",
            false,
            WireState::Raw,
        );
        play.assistant(
            "Now searching:",
            vec![tool_call("search", "toolu_r2", json!({ "pattern": "fn project", "path": "src" }))],
        );
        play.tool_result(
            "toolu_r2",
            "Found 1 match for pattern \"fn project\":\n\nsrc/project.rs:12: pub fn project(log: &[LogEntry]) -> Vec<ApiMessage> {",
            false,
            WireState::Raw,
        );
        play.assistant(
            "And listing:",
            vec![tool_call("list", "toolu_r3", json!({ "path": "src/tui" }))],
        );
        play.tool_result("toolu_r3", "All paths relative to src/tui:\n\n\ncomposer.rs\ndocument.rs\nprojection.rs", false, WireState::Raw);
        play.snapshot("transcript_file_tools");
    }

    /// A result longer than the collapse threshold: by default it shows the first
    /// few lines under a `▶` disclosure triangle with a `(+N lines)` hint, rather
    /// than hiding everything. This is the accordion's collapsed state.
    #[test]
    fn transcript_long_result_collapsed() {
        let mut play = Screenplay::new(80, 26);
        play.user("List the tui module.");
        play.assistant(
            "Here's the directory.",
            vec![tool_call("bash", "toolu_05", json!({ "command": "ls src/tui" }))],
        );
        let listing = (1..=20)
            .map(|n| format!("render_module_{n:02}.rs"))
            .collect::<Vec<_>>()
            .join("\n");
        play.tool_result("toolu_05", &listing, false, WireState::Raw);
        play.snapshot("transcript_long_result_collapsed");
    }

    /// The same long result after activating its disclosure triangle: `▼` and the
    /// full body, no `(+N lines)` hint. Drives the real `ExpandBlock` action.
    #[test]
    fn transcript_long_result_expanded() {
        let mut play = Screenplay::new(80, 32);
        play.user("List the tui module.");
        play.assistant(
            "Here's the directory.",
            vec![tool_call("bash", "toolu_05", json!({ "command": "ls src/tui" }))],
        );
        let listing = (1..=20)
            .map(|n| format!("render_module_{n:02}.rs"))
            .collect::<Vec<_>>()
            .join("\n");
        play.tool_result("toolu_05", &listing, false, WireState::Raw);
        play.expand_all();
        play.snapshot("transcript_long_result_expanded");
    }

    /// A distilled tool result — the `WireState::Distilled` path, where the
    /// transcript shows the curated summary in place of the raw output.
    #[test]
    fn transcript_distilled_result() {
        let mut play = Screenplay::new(80, 24);
        play.user("Summarize the dependency tree.");
        play.assistant(
            "Pulling the tree, then I'll distill it.",
            vec![tool_call(
                "bash",
                "toolu_03",
                json!({ "command": "cargo tree" }),
            )],
        );
        play.tool_result(
            "toolu_03",
            "efference v0.1.0\n├── anyhow v1.0.86\n├── clap v4.5.4\n│   ├── …600 more lines…",
            false,
            WireState::Distilled {
                keep: Keep::None,
                note: Some(
                    "12 direct deps; heaviest subtrees are tokio and trillium. Nothing outdated."
                        .into(),
                ),
            },
        );
        play.snapshot("transcript_distilled_result");
    }

    /// The same distilled result after flipping it to raw (`r`, DESIGN §10.5): the
    /// header gains a `· raw` marker and the body shows the full raw output the
    /// distillate replaced — the human's escape hatch back to what the tool actually
    /// returned, a pure client-side re-render (both forms are already local).
    #[test]
    fn transcript_raw_view_of_distilled() {
        let mut play = Screenplay::new(80, 24);
        play.user("Summarize the dependency tree.");
        play.assistant(
            "Pulling the tree, then I'll distill it.",
            vec![tool_call("bash", "toolu_03", json!({ "command": "cargo tree" }))],
        );
        play.tool_result(
            "toolu_03",
            "efference v0.1.0\n├── anyhow v1.0.86\n├── clap v4.5.4\n└── tokio v1.38.0",
            false,
            WireState::Distilled {
                keep: Keep::None,
                note: Some("12 direct deps; tokio + trillium are the heaviest.".into()),
            },
        );
        play.show_raw("toolu_03");
        play.snapshot("transcript_raw_view_of_distilled");
    }

    /// A pending approval gate, rendered inline at the tail call rather than as a
    /// bottom banner (DESIGN §10.5): the exchange shows a `⏸` marker and an
    /// `awaiting your approval …` affordance where the result will land — the gate
    /// reads as "after the call, before the result." Resolving it replaces the
    /// affordance with the real (or error) result.
    #[test]
    fn transcript_pending_approval_gate() {
        let mut play = Screenplay::new(80, 14);
        play.user("Clean the build directory.");
        play.assistant(
            "Sure — I'll remove it.",
            vec![tool_call("bash", "toolu_1", json!({ "command": "rm -rf ./build" }))],
        );
        play.pending_gate("toolu_1", "bash", json!({ "command": "rm -rf ./build" }));

        // The `⏸` marker and the affordance lead-in carry the user-accent (blue)
        // color — "your turn" — which the character grid can't show.
        let (mx, my) = play.find("⏸").expect("pending marker on screen");
        let (r, _g, b) = rgb(play.cell(mx, my).fg);
        assert!(b > r, "the pending marker should be a blue accent, got ({r},{b})");
        let (ax, ay) = play.find("awaiting").expect("affordance on screen");
        let (r, _g, b) = rgb(play.cell(ax, ay).fg);
        assert!(b > r, "the affordance lead-in should be a blue accent, got ({r},{b})");

        play.snapshot("transcript_pending_approval_gate");
    }

    /// Redirecting a gate: the call stays in the transcript with a message *instead
    /// of a result* (DESIGN §10.5). The loop clears the gate, pairs the orphaned
    /// `tool_use` with an error result ("not run — the user replied…"), and the
    /// human's words follow as a user turn — so the exchange reads `✗ bash …` over
    /// the breadcrumb, then the steer.
    #[test]
    fn transcript_redirected_gate() {
        let mut play = Screenplay::new(80, 16);
        play.user("Clean the build directory.");
        play.assistant(
            "Sure — I'll remove it.",
            vec![tool_call("bash", "toolu_1", json!({ "command": "rm -rf ./build" }))],
        );
        play.pending_gate("toolu_1", "bash", json!({ "command": "rm -rf ./build" }));
        // The human redirects: gate clears, the call gets an error result, the steer
        // follows as a user turn (the real loop's redirect path).
        play.event(crate::tui::TuiEvent::Gate(None));
        play.tool_result(
            "toolu_1",
            "Not run — the user replied instead of approving; see their next message.",
            true,
            WireState::Raw,
        );
        play.user("use `cargo clean` instead");
        play.snapshot("transcript_redirected_gate");
    }
}
