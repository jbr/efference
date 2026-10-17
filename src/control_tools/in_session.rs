//! The in-session set: the verbs a hosted session's toolbox mounts under
//! `agents__<verb>` (docs/tools.md), over the shared tool structs.

use super::{Archive, Control, Create, Decide, HandOff, Interrupt, Message, Status, Transcript};

mcplease::tools!(
    cli = false,
    modules = false,
    Control,
    (Create, "create"),
    (Message, "message"),
    (Decide, "decide"),
    (Interrupt, "interrupt"),
    (Archive, "archive"),
    (Status, "status"),
    (Transcript, "transcript"),
    (HandOff, "hand_off")
);
