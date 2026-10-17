//! The command-line set: `efference control <verb>` (docs/operating.md) — the agent
//! verbs, then the operator's route verbs. Each module re-exports the shared tool
//! struct.

use super::Control;

mcplease::tools!(
    Control,
    (Create, create, "create"),
    (Message, message, "message"),
    (Decide, decide, "decide"),
    (Interrupt, interrupt, "interrupt"),
    (Archive, archive, "archive"),
    (Status, status, "status"),
    (Transcript, transcript, "transcript"),
    (Settle, settle, "settle"),
    (State, state, "state"),
    (Pace, pace, "pace"),
    (Title, title, "title"),
    (Background, background, "background"),
    (Proposal, proposal, "proposal"),
    (ReviewLevel, review_level, "review_level"),
    (Supervisor, supervisor, "supervisor"),
    (Notify, notify, "notify"),
    (Entries, entries, "entries"),
    (Image, read_image, "image")
);
