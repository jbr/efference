//! Filmstrip frames replayed from a real recorded session (`fixtures/replay_session.jsonl`),
//! including the read-only view `efference view` opens one in.

use super::*;

/// Filmstrip frame 1: the first user turn, the assistant's `bash ls`, its
/// result, and the assistant's prose answer (entries 0–4 of the real session).
#[test]
fn replay_real_session_first_turn() {
    let entries = replay_fixture();
    let mut play = Screenplay::new(80, 30);
    play.replay(&entries[..=4]).scroll_to_top();
    play.snapshot("replay_real_session_first_turn");
}

/// Filmstrip frame 2: the tail of the whole real session, stream-followed to the
/// bottom. The session carried a spotter `gate` verdict (rendered beside the call it
/// judged, in the dim register), `ConfigEdit`s (audit-only — they project to nothing on
/// the transcript), and an `Ambient` notification (which *does* project). This pins how
/// the permission-bearing tail of a real session renders — including the case worth
/// seeing, a call the spotter gated that the human then approved, where the verdict is
/// the only surviving record of the disagreement.
#[test]
fn replay_real_session_tail() {
    let entries = replay_fixture();
    let mut play = Screenplay::new(80, 24);
    play.replay(&entries).pin_to_bottom();
    play.snapshot("replay_real_session_tail");
}

/// The same recorded session opened **read-only** (`efference view`, docs/tui.md):
/// identical transcript rendering, with the composer row gone, the strip reduced to
/// the client-only verbs, and the status bar naming the session it is reading. Wide
/// enough to dock the sidebar in a live session — which a read-only view drops,
/// since every section of it is fed by the `/state` poll.
#[test]
fn view_of_a_recorded_session_is_inert() {
    let entries = replay_fixture();
    let mut play = Screenplay::new(100, 24);
    play.viewing("session-42").replay(&entries).pin_to_bottom();
    play.snapshot("view_of_a_recorded_session_is_inert");
}

/// Read-only means nothing on screen can send. The strip carries no loop verb, and
/// the keys that would reach one — Enter (approve/submit), Esc (pause/interrupt) —
/// put nothing on any channel, because there is no daemon on the far end.
#[test]
fn a_read_only_view_offers_and_sends_nothing() {
    let entries = replay_fixture();
    let mut play = Screenplay::new(80, 24);
    play.viewing("session-42").replay(&entries).pin_to_bottom();

    assert!(
        play.find("close").is_some() && play.find("theme").is_some(),
        "the strip keeps the verbs that act on the client alone"
    );
    for verb in ["pace", "approve", "interrupt", "pause", "background"] {
        assert!(
            play.find(verb).is_none(),
            "{verb} is a loop verb — there is no loop behind a recorded log"
        );
    }
    assert!(
        play.find("read-only").is_some(),
        "the status bar says the view is inert"
    );

    // Keys that carry a send in a live session, from the strip and from the transcript.
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    play.route_key(KeyCode::Esc, KeyModifiers::NONE);
    play.route_key(KeyCode::Up, KeyModifiers::NONE);
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    play.route_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(play.took_turn().is_none(), "no turn was submitted");
    assert!(play.took_approval().is_none(), "no gate was resolved");
    assert!(play.took_pace().is_none(), "no pace command was pushed");
    assert!(!play.took_interrupt(), "no interrupt fired");
    assert!(!play.took_background(), "no background handoff fired");
}
