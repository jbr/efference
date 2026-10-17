//! The in-session set: the verbs a hosted session's toolbox mounts under
//! `agents__<verb>` (docs/tools.md). Each module re-exports the shared tool struct.

use super::Control;

mcplease::tools!(
    cli = false,
    Control,
    (Create, create, "create"),
    (Message, message, "message"),
    (Decide, decide, "decide"),
    (Interrupt, interrupt, "interrupt"),
    (Archive, archive, "archive"),
    (Status, status, "status"),
    (Transcript, transcript, "transcript"),
    (HandOff, hand_off, "hand_off")
);
