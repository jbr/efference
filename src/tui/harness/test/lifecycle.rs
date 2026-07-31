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

    // The arriving input is shown verbatim — never parsed, but not hidden either: the
    // identifying fields come first in JSON, so the head is the informative part.
    play.event(TuiEvent::ToolCallInput {
        fragment: "{\"comm".into(),
    });
    play.event(TuiEvent::ToolCallInput {
        fragment: "and\":\"ls src\"}".into(),
    });
    assert!(
        play.find(r#"{"command":"ls src"}"#).is_some(),
        "fragments accumulate into the raw input, shown in full as text — never parsed as a \
             complete document, and never truncated"
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
