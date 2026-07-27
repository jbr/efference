//! Inputs that used to panic or livelock the renderer.

use super::*;

/// A running tool streams raw process bytes into the live tail
/// ([`projection`](crate::tui::projection) renders `live.tools` directly).
/// That output carries control characters — ANSI color escapes (`\x1b[…m`),
/// carriage returns from progress bars — which a durable, JSON-encoded log
/// never holds, so this path is the only one that can deliver them to a cell.
/// Ratatui's `cell_width` debug-asserts on a control char and panics; the live
/// tail is replayed verbatim on every reattach, so the crash is deterministic.
/// Rendering must instead be total: control chars fold to a glyph, no panic.
#[test]
fn live_tool_output_with_control_chars_does_not_panic() {
    use crate::tui::TuiEvent;
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
    assert!(
        screen.contains("done"),
        "text after the token still renders"
    );
}
