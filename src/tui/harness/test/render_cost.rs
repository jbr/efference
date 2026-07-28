//! What a frame costs as the transcript grows (docs/tui.md).
//!
//! The transcript is a growing document repainted ~33 times a second, so the thing
//! worth pinning is not a duration — it is that the per-frame work stays bounded by
//! the *viewport*, not by the log. `laid_out` counts the top-level nodes a walk
//! actually laid out; everything else was reused. These assert that number stays
//! flat as the log gets 8× longer, which is what a timing test would be trying to
//! say anyway, without being a timing test.

use super::*;

/// A session of `turns` tool exchanges, stream-followed to the bottom the way a
/// live session sits.
fn session(turns: usize) -> Screenplay<'static> {
    let mut play = Screenplay::new(120, 40);
    for i in 0..turns {
        play.user(&format!("step {i}"));
        play.tool_exchange(
            &format!("call-{i}"),
            crate::bash::NAME,
            &format!("cargo test --test t{i}"),
            // Several lines, so a re-render of one result is visible in the frame.
            &(1..=12)
                .map(|n| format!("test t{i}::case_{n} ... ok"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    play.pin_to_bottom();
    play
}

/// One streamed token against a long transcript costs the same as against a short
/// one. Before the layout cache this walk was the whole document every frame, which
/// is what made a long session visibly lag behind the stream.
#[test]
fn a_streamed_token_costs_the_viewport_not_the_log() {
    let mut short = session(25);
    let mut long = session(200);

    let delta = |play: &mut Screenplay<'_>| {
        play.event(TuiEvent::AssistantDelta("another token ".into()));
        let _ = play.draw();
        play.state.document.laid_out
    };

    // Warm both, then measure a steady-state streaming frame.
    delta(&mut short);
    delta(&mut long);
    let short_cost = delta(&mut short);
    let long_cost = delta(&mut long);

    assert!(
        long_cost <= short_cost + 2,
        "an 8× longer log laid out {long_cost} nodes against the short log's \
         {short_cost} — the per-frame cost is tracking the log, not the viewport"
    );
    assert!(
        long_cost < 40,
        "{long_cost} nodes laid out for one streamed token into a 40-row viewport"
    );
}

/// Scrolling is bounded the same way: moving the viewport lays out what enters it,
/// not everything above it. (Every node above the viewport must still be *walked* to
/// know where the viewport starts — that walk is now integer arithmetic.)
#[test]
fn scrolling_costs_the_viewport_not_the_log() {
    let mut play = session(200);
    let _ = play.draw();

    let max = play.state.max_scroll();
    play.state.set_scroll_offset(max / 2);
    let _ = play.draw();
    let jumped = play.state.document.laid_out;

    play.state.set_scroll_offset(max / 2 + 3);
    let _ = play.draw();
    let nudged = play.state.document.laid_out;

    assert!(
        jumped < 40 && nudged < 40,
        "a scroll laid out {jumped} then {nudged} nodes for a 40-row viewport"
    );
}

/// **A block is syntax-highlighted once, not once a frame.** A *visible* top-level
/// node is re-laid-out on every paint, and a multi-screen code block is one node —
/// so without the memo, syntect re-parses the whole block ~33 times a second while
/// the operator scrolls past it. Worse, parse state has to be carried from the
/// block's first line each time, so the cost peaks exactly where the operator has
/// scrolled furthest into it: the observed symptom was a long highlighted block
/// scrolling more slowly than anything else.
#[test]
fn a_block_is_highlighted_once_not_once_a_frame() {
    let mut play = Screenplay::new(100, 30);
    play.user("read it");
    play.assistant(
        "here",
        vec![tool_call(
            "read",
            "call-hl",
            serde_json::json!({ "paths": ["src/tui/state.rs"] }),
        )],
    );
    let body: String = (1..=2000)
        .map(|n| format!("    let value_{n} = compute(&state, {n});\n"))
        .collect();
    play.tool_result(
        "call-hl",
        &format!(
            "==MK BEGIN /src/tui/state.rs, LENGTH: 9 MK==\n{body}==MK END /src/tui/state.rs MK=="
        ),
        false,
        crate::log::WireState::Raw,
    );

    // The first paint pays for the block, once.
    let _ = play.draw();
    let first = play.state.document.highlighted;
    assert!(first > 0, "the first paint has to actually highlight it");

    // Every later frame — including one that scrolls — pays nothing.
    let _ = play.draw();
    assert_eq!(
        play.state.document.highlighted, 0,
        "a repaint re-parsed the block"
    );

    for offset in [1900, 1500, 900, 300, 0] {
        play.state.set_scroll_offset(offset);
        let _ = play.draw();
        assert_eq!(
            play.state.document.highlighted, 0,
            "scrolling to row {offset} re-parsed the block"
        );
    }
}

/// The memo is keyed on what the colours are a function of, so a theme change
/// misses it rather than painting the old theme's colours forever.
#[test]
fn a_theme_change_misses_the_highlight_memo() {
    let mut play = Screenplay::new(100, 30);
    play.user("read it");
    play.assistant(
        "here",
        vec![tool_call(
            "read",
            "call-hl",
            serde_json::json!({ "paths": ["a.rs"] }),
        )],
    );
    play.tool_result(
        "call-hl",
        "==MK BEGIN /a.rs, LENGTH: 9 MK==\nfn main() {}\n==MK END /a.rs MK==",
        false,
        crate::log::WireState::Raw,
    );
    let _ = play.draw();
    let _ = play.draw();
    assert_eq!(play.state.document.highlighted, 0, "warm");

    let themes = crate::tui::render_context::RenderContext::available_themes();
    let other = themes
        .iter()
        .find(|t| Some(t.as_str()) != play.state.current_theme_name.as_deref())
        .expect("a second theme to switch to");
    play.state.apply_theme(other).expect("theme applies");
    play.state.document.invalidate_all();
    let _ = play.draw();
    assert!(
        play.state.document.highlighted > 0,
        "the new theme's colours must be recomputed, not served from the memo"
    );
}

/// The reverse guarantee: a node whose content changed *is* re-laid-out. A cache
/// that never invalidated would pass the tests above and paint stale rows forever.
/// The raw flip is the change to drive it with — it is the one per-entry toggle
/// that alters what an already-committed entry renders.
#[test]
fn a_re_rendered_result_drops_its_cached_layout() {
    let mut play = Screenplay::new(120, 40);
    play.user("run it");
    play.assistant(
        "on it",
        vec![crate::tui::harness::test::tool_call(
            crate::bash::NAME,
            "call-1",
            serde_json::json!({ "command": "cargo tree" }),
        )],
    );
    play.tool_result(
        "call-1",
        "RAW-ONE\nRAW-TWO\nRAW-THREE",
        false,
        crate::log::WireState::Distilled {
            keep: crate::log::Keep::None,
            note: Some("three deps".into()),
        },
    );
    play.pin_to_bottom();
    let before = play.render();
    assert!(
        play.state.document.layouts.iter().all(Option::is_some),
        "a drawn document has a layout for every node"
    );

    play.show_raw("call-1");
    assert!(
        play.state.document.layouts.iter().any(Option::is_none),
        "re-projecting an entry must drop that node's cached height"
    );
    assert_ne!(
        before,
        play.render(),
        "and the next frame must show the re-rendered entry"
    );
}
