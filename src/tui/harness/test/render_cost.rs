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
            // Long enough to be collapsible, so the expand test has something to expand.
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

/// The reverse guarantee: a node whose content changed *is* re-laid-out. A cache
/// that never invalidated would pass the tests above and paint stale rows forever.
#[test]
fn expanding_a_block_drops_its_cached_layout() {
    let mut play = session(3);
    let before = play.render();
    assert!(
        play.state.document.layouts.iter().all(Option::is_some),
        "a drawn document has a layout for every node"
    );

    play.expand_all();
    assert!(
        play.state.document.layouts.iter().any(Option::is_none),
        "expanding a block must drop that node's cached height"
    );
    assert_ne!(
        before,
        play.render(),
        "and the next frame must show the expanded body"
    );
}
