//! Turn lifecycle as the user sees it: pending echo, streamed output kinds, commit.

use super::*;

/// The operator-facing half of mid-task delivery (docs/tui.md): a
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
    play.commit_pending();

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

/// **A turn with no prose is still visibly working** (docs/tui.md). Without thinking
/// and tool-call input reaching the TUI as their own frames, the window between
/// submitting a turn and the first prose token is blank, and blank is
/// indistinguishable from hung. Each output kind shows what it honestly can: thinking its
/// *tail* (what the model is working through now), a tool call its name, then the
/// bytes of input received.
#[test]
fn a_prose_less_generation_is_visible() {
    let mut play = Screenplay::new(80, 12);
    play.user("Count the modules.");

    play.event(TuiEvent::ThinkingDelta("I should look at the ".into()));
    play.event(TuiEvent::ThinkingDelta("src directory first".into()));
    assert!(
        play.find("thinking").is_some(),
        "streamed thinking is not silent"
    );
    assert!(
        play.find("I should look at the ").is_some() && play.find("src directory first").is_some(),
        "…and shows the thinking in full, not a truncated preview — the point is to watch the \
             work as it happens"
    );

    // The call is named as soon as it opens, before any argument bytes arrive.
    play.calling(0, "toolu_live", "bash");
    assert!(
        play.find("› bash").is_some(),
        "the pending call is named before its input streams"
    );
    assert!(
        play.find("src directory first").is_some(),
        "thinking stays in the transcript when the model moves on to acting — it was a \
             single-slot preview once, so a tool call replaced it faster than it \
             could be read"
    );

    // Before a single pair has closed there is no structure to claim, so the raw
    // characters are shown — they claim nothing, which is what makes them safe.
    play.call_input(0, "{\"comm");
    assert!(
        play.find(r#"{"comm"#).is_some(),
        "with nothing structural arrived yet, the raw bytes are shown rather than nothing"
    );

    // Once the pair closes it reads as an argument, through the same renderer a
    // committed call uses — so the header fills in rather than changing shape.
    play.call_input(0, "and\":\"ls src\"}");
    assert!(
        play.find("ls src").is_some(),
        "a closed argument renders as an argument"
    );
    assert!(
        play.find(r#"{"command""#).is_none(),
        "…and the JSON envelope it arrived in is no longer what the operator reads"
    );

    // Prose is its own evidence of progress, so it replaces the calling line.
    play.event(TuiEvent::AssistantDelta("Looking now.".into()));
    assert!(
        play.find("calling bash").is_none(),
        "prose supersedes the calling line"
    );
    assert!(play.find("Looking now.").is_some());
}

/// The streamed-output line is *live*, not transcript: once the assistant entry commits, the committed
/// record is the thing to read and the live tail has nothing left to report. (Rendering committed
/// thinking in the transcript is a separate unit — docs/transport.md.)
#[test]
fn a_committed_turn_clears_the_live_tail() {
    let mut play = Screenplay::new(80, 12);
    play.user("Count the modules.");
    play.event(TuiEvent::ThinkingDelta("weighing the options".into()));
    assert!(play.find("thinking").is_some());

    play.assistant("Twelve.", vec![]);
    assert!(
        play.find("thinking").is_none(),
        "the committed turn supersedes the live tail"
    );
    assert!(play.find("Twelve.").is_some());
}

/// The live tail's line spacing. Every live renderer — a running tool's output
/// triage, streamed thinking, an arriving tool input — models one source line as one
/// `Paragraph` child of its entry, and an entry separates its children by a blank
/// row (markdown block spacing). So a multi-line live block renders double-spaced,
/// which reads as the stream having inserted newlines it did not send.
#[test]
fn the_live_tail_is_not_double_spaced() {
    let mut play = Screenplay::new(80, 24);
    play.user("Run the tests.");
    play.tool_output("call-live", "one\ntwo\nthree\n");
    play.pin_to_bottom();
    play.snapshot("live_tool_output_spacing");
}

/// The streaming write: the path is a settled argument and reads as one, while the
/// contents are still arriving and read as content (docs/tui.md).
#[test]
fn a_streaming_write_shows_the_file_it_is_writing() {
    let mut play = Screenplay::new(80, 24);
    play.user("Write the renderer.");
    play.calling(0, "toolu_write", "write");
    // Emitted the way a model emits it: identifying arguments first, contents last
    // and still open.
    play.call_input(
        0,
        r#"{"intent":"write the renderer","path":"src/render.rs","contents":"fn main() {\n    let x = 1;\n"#,
    );
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
    play.calling(0, "toolu_write", "write");
    play.call_input(0, r#"{"path":"a.rs","contents":""#);
    for _ in 0..40 {
        play.call_input(0, r#"    let value = compute(state);\n"#);
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
        play.call_input(0, r#"    let value = compute(state);\n"#);
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

/// **Committed thinking keeps the model's structure.** It is the copy a human
/// reads afterwards, at their own pace (docs/tui.md), and it is not flowing prose:
/// it holds lists and indentation, which reflowing to paragraphs destroys. It also
/// has to match the live render, which renders the same text the same way —
/// otherwise the block visibly reshapes at the moment the turn commits.
///
/// Thinking renders as markdown, so a list is a *list*: the invariant is that the
/// items stay on separate rows, not that the source's `- ` prefix survives verbatim.
/// The regression this guards is `- a\n- b` reflowed to `- a - b`.
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
        play.find("reflow to paragraphs").is_some(),
        "the first list item is on the screen"
    );
    assert!(
        play.find("reflow to paragraphs keep the lines").is_none(),
        "…and not joined to its neighbour, which is what reflowing did to it"
    );
    assert!(
        play.find("keep the lines").is_some(),
        "the second item is on its own row"
    );
    play.snapshot("committed_thinking");
}

/// **A fenced code block inside thinking renders as code.** This is the visible
/// reason to route thinking through the markdown renderer at all: as literal
/// lines the fence markers were text and the code had no highlighting.
///
/// It is also where the dimmed register has to make a choice, and the one taken
/// is that code keeps its hue separation while giving up brightness — dimming it
/// flat would make the block legible as *structure* and useless as *code*
/// (docs/tui.md). The colors are not visible in the no-color snapshot grid, so
/// what this pins is the structure; `src/tui/span_style/test.rs` covers the
/// blend itself.
#[test]
fn thinking_renders_a_fenced_code_block_as_code() {
    let mut play = Screenplay::new(80, 24);
    play.user("Which signature?");
    play.assistant_thinking(
        "The projection entry point:\n\n```rust\nfn project(log: &[LogEntry]) -> Vec<ApiMessage>\n```\n\nThat one.",
        "That one.",
    );
    play.pin_to_bottom();
    assert!(
        play.find("fn project(log: &[LogEntry]) -> Vec<ApiMessage>")
            .is_some(),
        "the code line renders"
    );
    assert!(
        play.find("```").is_none(),
        "the fence markers are structure, not text to print"
    );
    play.snapshot("thinking_code_block");
}

/// **Streamed thinking persists, and the commit does not reshape it.**
///
/// Streamed thinking is accumulated live-tail content, not a single-slot preview that
/// `tool_call_start` would replace — a turn that thinks briefly and then acts keeps
/// its reasoning on screen. The same builder renders the live and the committed
/// block, so the handoff is invisible (docs/tui.md).
#[test]
fn streamed_thinking_persists_and_does_not_reshape_on_commit() {
    let thought = "Two options here:\n- reflow to paragraphs\n- keep the lines\n\nThe second one.";

    let mut play = Screenplay::new(80, 24);
    play.user("Which approach?");
    for word in thought.split_inclusive(' ') {
        play.event(TuiEvent::ThinkingDelta(word.to_string()));
    }
    play.pin_to_bottom();
    let live = transcript_rows(&play.render());

    // Acting does not wipe it.
    play.calling(0, "toolu_write", "write");
    assert!(
        play.find("keep the lines").is_some(),
        "a tool call starting wiped the thinking that led to it"
    );

    // The committed turn carries the same thinking, and must render it the same way.
    let mut committed = Screenplay::new(80, 24);
    committed.user("Which approach?");
    committed.assistant_thinking(thought, "");
    committed.pin_to_bottom();
    assert_eq!(
        live,
        transcript_rows(&committed.render()),
        "the block changed shape when the turn committed — the transcript visibly \
         rewrites itself at that moment even though both renders are correct"
    );
}

/// **A live block's toggle survives the next stream event.**
///
/// A running tool's output shows a peek by default (docs/tui.md), and the operator
/// can open it to read the whole thing. The choice is held against the entry's
/// `call_id`: an unkeyed triage entry has nothing durable to record it against, so
/// the next arriving fragment re-projects the block and snaps it shut. The
/// `call_id` is on the tail already (`tool_output` is demuxed by it).
#[test]
fn a_streaming_block_opened_full_stays_open_across_stream_events() {
    let mut play = Screenplay::new(80, 30);
    play.user("Run the suite.");
    play.running_bash("toolu_run", "cargo test");
    for n in 1..=14 {
        play.tool_output("toolu_run", &format!("line {n}\n"));
    }
    play.pin_to_bottom();
    assert!(
        play.find("running · 14 lines, last 10").is_some(),
        "the default is the triage"
    );
    assert!(play.find("line 1\n").is_none(), "…so the head is not shown");

    // Focus the triage (the last unit) and fire its one verb.
    play.route_key(KeyCode::Up, KeyModifiers::NONE); // composer → strip
    play.route_key(KeyCode::Up, KeyModifiers::NONE); // strip → transcript, last unit
    assert_eq!(
        play.state
            .block_menu_items()
            .iter()
            .map(|item| item.label)
            .collect::<Vec<_>>(),
        vec!["full"],
        "a running tool's peek carries the peek/full verb"
    );
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        play.find("line 1").is_some(),
        "the verb opened the whole output"
    );

    // The next streamed fragment, which re-projects the block.
    play.tool_output("toolu_run", "line 15\n");
    assert!(
        play.find("line 1").is_some(),
        "the next streamed fragment re-collapsed the block the operator opened"
    );
    assert!(
        play.find("line 15").is_some(),
        "…and the new line landed in the open view"
    );
    play.pin_to_bottom();
    play.snapshot("running_peek_opened_full");

    // The committed result renders whole, so the live-only flag retires with it.
    play.tool_result("toolu_run", "line 1\nline 2\n", false, WireState::Raw);
    assert!(
        !play.state.view.is_expanded("toolu_run"),
        "the peek/full flag belongs to output still arriving"
    );
}

/// **Two calls in one response both render.** The live tail held one slot for the
/// call in flight, so the second `tool_call_start` replaced the first and the
/// arguments the operator was watching arrive vanished mid-flight. Every delta names
/// the content block it belongs to (docs/control-api.md), so each call's fragments
/// bind to the call that opened that block.
#[test]
fn two_calls_in_one_response_each_keep_their_arriving_arguments() {
    let mut play = Screenplay::new(80, 24);
    play.user("Read both files.");

    play.calling(0, "toolu_a", "read");
    play.call_input(0, r#"{"path":"src/log.rs","#);
    assert!(play.find("src/log.rs").is_some(), "the first call renders");

    play.calling(1, "toolu_b", "bash");
    play.call_input(1, r#"{"command":"ls src","#);
    play.pin_to_bottom();

    assert!(
        play.find("src/log.rs").is_some(),
        "the second call starting replaced the first, losing what it had shown"
    );
    assert!(play.find("ls src").is_some(), "the second call renders too");

    // A fragment names its block, so it extends the call it belongs to rather than
    // whichever one arrived last.
    play.call_input(0, r#""intent":"read the log module"}"#);
    assert!(
        play.find("read the log module").is_some(),
        "the fragment landed on block 0's call"
    );
    play.snapshot("parallel_live_calls");
}

/// The transcript half of a rendered frame, without the bottom chrome. The strip and
/// status bar legitimately differ between a streaming turn and a settled one (`◍
/// streaming…`, the interrupt verbs), which is not the transcript reshaping.
fn transcript_rows(frame: &str) -> String {
    frame
        .lines()
        .take_while(|line| !line.contains("help ?"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Recall (docs/tui.md): Ctrl-R asks the daemon to withdraw the most recent queued turn,
/// addressed by the submission id the TUI minted. Nothing moves until the daemon answers — and
/// what it answers is what the composer gets, prepended ahead of the draft already there.
#[test]
fn recall_asks_by_submission_id_and_splices_what_came_back() {
    let mut play = Screenplay::new(60, 12);
    play.type_str("queued message");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    let submitted = play.state.pending_turns[0].clone();
    assert_eq!(play.took_turn().as_deref(), Some("queued message"));

    // A draft is already in the composer when the recall goes out.
    play.type_str("draft");
    play.route_key(KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(
        play.took_recall().as_deref(),
        Some(submitted.submission_id.as_str()),
        "the recall addresses the exact submission"
    );
    assert!(
        play.find("⋯").is_some(),
        "the echo stays until the daemon answers"
    );

    play.event(TuiEvent::Recalled(vec![QueuedTurn::from(&submitted)]));
    assert!(
        play.state.pending_turns.is_empty(),
        "the echo is gone once the withdraw is confirmed"
    );
    assert_eq!(play.state.composer.text(), "queued message\ndraft");
}

/// The lost race: the loop dequeued the turn first, so the daemon pops nothing. The composer is
/// left alone rather than handed back text that is already on its way to the model.
#[test]
fn a_recall_that_popped_nothing_leaves_the_composer_alone() {
    let mut play = Screenplay::new(60, 12);
    play.type_str("already sent");
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    play.route_key(KeyCode::Char('r'), KeyModifiers::CONTROL);
    play.event(TuiEvent::Recalled(Vec::new()));
    assert_eq!(play.state.composer.text(), "");
    assert_eq!(play.state.pending_turns.len(), 1, "the echo is untouched");
}
