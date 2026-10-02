use super::Screenplay;
use crate::log::{Keep, Stance, ToolCall, WireState};
use crate::tui::{PaceView, TuiEvent};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::{Color, Modifier};
use serde_json::json;

/// A realistic `edit` result (real format from `.efference/*.jsonl`):
/// preamble lines, then a `===DIFF===` section with ` `/`+`/`-` line prefixes.
const EDIT_RESULT: &str = "Applied replace — the file has been updated.\n\
        Note: the editor applies a consistent formatting style to the entire file, including your edit\n\n\
        ===DIFF===\n\n\
        \x20    /// True when the buffer holds no non-whitespace text.\n\
        \x20    pub(super) fn is_empty(&self) -> bool {\n\
        -        self.lines.iter().all(|l| l.iter().all(|c| c.is_whitespace()))\n\
        +        self.lines\n\
        +            .iter()\n\
        +            .all(|l| l.iter().all(|c| c.is_whitespace()))\n\
        \x20    }";

/// Builds a transcript whose last entry is an `edit` result carrying
/// [`EDIT_RESULT`], so both the snapshot and the color test share one setup.
fn edit_scenario(w: u16, h: u16) -> Screenplay<'static> {
    let mut play = Screenplay::new(w, h);
    play.user("Wrap that long line.");
    play.assistant(
        "Here's the reformatted version.",
        vec![ToolCall::new(
            "edit",
            "toolu_08",
            json!({
                "file_path": "src/tui/composer.rs",
                "operation": "replace",
                "anchor": "fn is_empty",
                "content": "…",
            }),
        )],
    );
    play.tool_result("toolu_08", EDIT_RESULT, false, WireState::Raw);
    play
}

/// Unpack an (asserted) RGB cell color into channels.
fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        other => panic!("expected an RGB diff color, got {other:?}"),
    }
}

/// Load the checked-in replay fixture (`fixtures/replay_session.jsonl`, whose
/// entries are lifted verbatim from a real `.efference/` session under a header
/// line added to make it a readable log) through the same `persist::load`
/// a live attach uses. Every state a replay of it produces is reachable *by
/// construction* — it came off a real daemon, not a test author's hand.
fn replay_fixture() -> Vec<crate::log::LogEntry> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/tui/fixtures/replay_session.jsonl");
    crate::persist::load(&path).expect("the replay fixture loads")
}

mod ask;
mod attachments;
mod hover;
mod lifecycle;
mod navigation;
mod pace;
mod render_cost;
mod replay;
mod retry;
mod robustness;
mod sidebar;
mod transcript;
