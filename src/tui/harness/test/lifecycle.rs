//! Turn lifecycle as the user sees it: pending echo, generation phase, commit.

use super::*;

/// The seam-drain's operator-facing half (docs/tui.md): a
/// submitted turn echoes **immediately** as a pending unit — dimmed, led by a `⋯`
/// marker — so a mid-trajectory message never vanishes into the void. When its
/// matching `User` entry streams back (the entry *is* on the wire), the echo
/// resolves: the marker drops and it renders as a normal committed turn.
#[test]
fn submitted_turn_echoes_pending_then_resolves_on_commit() {
    let mut play = Screenplay::new(60, 12);
    play.type_str("hello there");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);

    // The optimistic echo is on screen right away, with the pending marker…
    assert!(
        play.find("⋯").is_some(),
        "the pending marker shows immediately on submit"
    );
    let (tx, ty) = play.find("hello there").expect("pending echo text visible");
    let pending_fg = play.cell(tx, ty).fg;

    // …and the composer was cleared (the text isn't sitting in the input anymore,
    // only in the transcript echo — one occurrence on screen).
    // Now the daemon's `/events` delivers the committed User entry.
    play.user("hello there");

    assert!(
        play.find("⋯").is_none(),
        "the pending marker drops once the User entry commits"
    );
    let (cx, cy) = play.find("hello there").expect("committed turn present");
    let committed_fg = play.cell(cx, cy).fg;
    assert_ne!(
        pending_fg, committed_fg,
        "the in-flight echo is dimmed; the committed turn renders in the normal voice"
    );
}

/// **A turn with no prose is still visibly working** (docs/tui.md). Thinking
/// and a tool call's streaming input used to reach the TUI not at all — the sink carried only
/// `text_delta` — so the whole window between submitting a turn and the first prose token was
/// blank, and blank is indistinguishable from hung. Each phase shows what it honestly can:
/// thinking its *tail* (what the model is working through now), a tool call its name, then the
/// bytes of input received.
#[test]
fn a_prose_less_generation_phase_is_visible() {
    let mut play = Screenplay::new(80, 12);
    play.user("Count the modules.");

    play.event(TuiEvent::ThinkingDelta("I should look at the ".into()));
    play.event(TuiEvent::ThinkingDelta("src directory first".into()));
    assert!(
        play.find("thinking").is_some(),
        "a thinking phase is not silent"
    );
    assert!(
        play.find("I should look at the ").is_some() && play.find("src directory first").is_some(),
        "…and shows the thinking in full, not a truncated preview — the point is to watch the \
             work as it happens"
    );

    // The call is named as soon as it opens, before any argument bytes arrive.
    play.event(TuiEvent::ToolCallStart {
        name: "bash".into(),
    });
    assert!(
        play.find("calling bash").is_some(),
        "the pending call is named before its input streams"
    );
    assert!(
        play.find("src directory first").is_none(),
        "acting supersedes thinking — one phase at a time"
    );

    // Before a single pair has closed there is no structure to claim, so the raw
    // characters are shown — they claim nothing, which is what makes them safe.
    play.event(TuiEvent::ToolCallInput {
        fragment: "{\"comm".into(),
    });
    assert!(
        play.find(r#"{"comm"#).is_some(),
        "with nothing structural arrived yet, the raw bytes are shown rather than nothing"
    );

    // Once the pair closes it reads as an argument, through the same renderer a
    // committed call uses — so the header fills in rather than changing shape.
    play.event(TuiEvent::ToolCallInput {
        fragment: "and\":\"ls src\"}".into(),
    });
    assert!(
        play.find("ls src").is_some(),
        "a closed argument renders as an argument"
    );
    assert!(
        play.find(r#"{"command""#).is_none(),
        "…and the JSON envelope it arrived in is no longer what the operator reads"
    );

    // Prose is its own evidence of progress, so it replaces the phase line.
    play.event(TuiEvent::AssistantDelta("Looking now.".into()));
    assert!(
        play.find("calling bash").is_none(),
        "prose supersedes phase"
    );
    assert!(play.find("Looking now.").is_some());
}

/// The phase line is *live*, not transcript: once the assistant entry commits, the committed
/// record is the thing to read and the phase has nothing left to report. (Rendering committed
/// thinking in the transcript is a separate unit — docs/transport.md.)
#[test]
fn a_committed_turn_clears_the_live_phase() {
    let mut play = Screenplay::new(80, 12);
    play.user("Count the modules.");
    play.event(TuiEvent::ThinkingDelta("weighing the options".into()));
    assert!(play.find("thinking").is_some());

    play.assistant("Twelve.", vec![]);
    assert!(
        play.find("thinking").is_none(),
        "the committed turn supersedes the live phase"
    );
    assert!(play.find("Twelve.").is_some());
}

/// The live tail's line spacing. Every live renderer — a running tool's output
/// peek, a thinking phase, an arriving tool input — models one source line as one
/// `Paragraph` child of its entry, and an entry separates its children by a blank
/// row (markdown block spacing). So a multi-line live block renders double-spaced,
/// which reads as the stream having inserted newlines it did not send.
#[test]
fn the_live_tail_is_not_double_spaced() {
    let mut play = Screenplay::new(80, 24);
    play.user("Run the tests.");
    play.event(TuiEvent::ToolOutput {
        call_id: "call-live".into(),
        chunk: "one\ntwo\nthree\n".into(),
    });
    play.pin_to_bottom();
    play.snapshot("live_tool_output_spacing");
}

/// The streaming write: the path is a settled argument and reads as one, while the
/// contents are still arriving and read as content (docs/tui.md).
#[test]
fn a_streaming_write_shows_the_file_it_is_writing() {
    let mut play = Screenplay::new(80, 24);
    play.user("Write the renderer.");
    play.event(TuiEvent::ToolCallStart {
        name: "write".into(),
    });
    // Emitted the way a model emits it: identifying arguments first, contents last
    // and still open.
    play.event(TuiEvent::ToolCallInput {
        fragment: r#"{"intent":"write the renderer","path":"src/render.rs","contents":"fn main() {\n    let x = 1;\n"#
            .into(),
    });
    play.pin_to_bottom();
    play.snapshot("streaming_write_call");
}

/// **Scrolling up during a stream stays where the operator put it.**
///
/// A re-projection drops the layout cache, and `ui_loop` re-projects before it
/// drains an input burst — so a scroll event arriving mid-stream sees an
/// invalidated layout. When `max_scroll` answered 0 there, `offset >= max_scroll`
/// read as "at the bottom", which re-armed stream-following: scrolling down a notch
/// from many screens up snapped straight to the end, losing the place being read.
#[test]
fn scrolling_down_mid_stream_does_not_snap_to_the_end() {
    let mut play = Screenplay::new(80, 20);
    play.user("write it");
    play.event(TuiEvent::ToolCallStart {
        name: "write".into(),
    });
    play.event(TuiEvent::ToolCallInput {
        fragment: r#"{"path":"a.rs","contents":""#.into(),
    });
    for _ in 0..40 {
        play.event(TuiEvent::ToolCallInput {
            fragment: r#"    let value = compute(state);\n"#.into(),
        });
    }
    play.pin_to_bottom();
    let _ = play.draw();

    // Scroll up a little: stream-following detaches and a place is being read.
    play.state.viewport.stuck_to_bottom = false;
    let reading_at = play.state.max_scroll() - 3;
    play.state.set_scroll_offset(reading_at);
    let _ = play.draw();

    // Many more screens arrive. The event loop re-projects before handling input,
    // which is what leaves the layout cache invalid at the moment a scroll lands.
    for _ in 0..200 {
        play.event(TuiEvent::ToolCallInput {
            fragment: r#"    let value = compute(state);\n"#.into(),
        });
    }
    play.state.flush_reproject();
    assert!(
        play.state.viewport.cached_layout.is_none(),
        "the scenario requires the layout to be invalid when the scroll arrives"
    );

    // One notch down, exactly as the mouse handler does it.
    play.state
        .set_scroll_offset(play.state.viewport.scroll_offset.saturating_add(1));
    play.state.viewport.stuck_to_bottom =
        play.state.viewport.scroll_offset >= play.state.max_scroll();

    assert!(
        !play.state.viewport.stuck_to_bottom,
        "one notch down from many screens up re-armed stream-following"
    );
    assert_eq!(
        play.state.viewport.scroll_offset,
        reading_at + 1,
        "the viewport moved by a notch, not to the end"
    );
    let _ = play.draw();
    assert_eq!(
        play.state.viewport.scroll_offset,
        reading_at + 1,
        "and the next frame left it there"
    );
}

/// **Committed thinking keeps the model's line structure.** It is the copy a human
/// reads afterwards, at their own pace (docs/tui.md), and it is not flowing prose:
/// it holds lists and indentation, which reflowing to paragraphs destroys. It also
/// has to match the live phase, which shows the same text line by line — otherwise
/// the block visibly reshapes at the moment the turn commits.
#[test]
fn committed_thinking_keeps_the_lines_the_model_emitted() {
    let mut play = Screenplay::new(80, 24);
    play.user("Which approach?");
    play.assistant_thinking(
        "Two options here:\n- reflow to paragraphs\n- keep the lines\n\nThe second one.",
        "The second one.",
    );
    play.pin_to_bottom();
    assert!(
        play.find("- reflow to paragraphs").is_some(),
        "a list item is on its own row, not joined to its neighbour"
    );
    assert!(
        play.find("- reflow to paragraphs - keep the lines")
            .is_none(),
        "…which is what reflowing to a paragraph did to it"
    );
    play.snapshot("committed_thinking");
}
