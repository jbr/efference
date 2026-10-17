//! The command-line set: `efference control <verb>` (docs/operating.md) — the agent
//! verbs, then the operator's route verbs, over the shared tool structs.

use super::{
    Archive, Background, CliStatus as Status, Control, Create, Decide, Entries, Image, Interrupt,
    Message, Notify, Pace, Proposal, ReviewLevel, Settle, State, Supervisor, Title, Transcript,
};

mcplease::tools!(
    modules = false,
    Control,
    (Create, "create"),
    (Message, "message"),
    (Decide, "decide"),
    (Interrupt, "interrupt"),
    (Archive, "archive"),
    (Status, "status"),
    (Transcript, "transcript"),
    (Settle, "settle"),
    (State, "state"),
    (Pace, "pace"),
    (Title, "title"),
    (Background, "background"),
    (Proposal, "proposal"),
    (ReviewLevel, "review_level"),
    (Supervisor, "supervisor"),
    (Notify, "notify"),
    (Entries, "entries"),
    (Image, "image")
);
