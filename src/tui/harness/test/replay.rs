//! Filmstrip frames replayed from a real recorded session (`fixtures/replay_session.jsonl`).

use super::*;

/// Filmstrip frame 1: the first user turn, the assistant's `bash ls`, its
/// result, and the assistant's prose answer (entries 0–4 of the real session).
#[test]
fn replay_real_session_first_turn() {
    let entries = replay_fixture();
    let mut play = Screenplay::new(80, 30);
    play.replay(&entries[..=4]);
    play.snapshot("replay_real_session_first_turn");
}

/// Filmstrip frame 2: the tail of the whole real session, stream-followed to the
/// bottom. The session carried a spotter `gate` verdict and `ConfigEdit`s (both
/// audit-only — they project to nothing on the transcript) plus an `Ambient`
/// notification (which *does* project). This pins how the permission-bearing tail
/// of a real session renders, and that the audit-only payloads stay invisible.
#[test]
fn replay_real_session_tail() {
    let entries = replay_fixture();
    let mut play = Screenplay::new(80, 24);
    play.replay(&entries).pin_to_bottom();
    play.snapshot("replay_real_session_tail");
}
