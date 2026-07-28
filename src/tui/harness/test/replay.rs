//! Filmstrip frames replayed from a real recorded session (`fixtures/replay_session.jsonl`).

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
