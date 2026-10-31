//! A reply that never completed and the session's own lifecycle each read as one line
//! (docs/control-api.md).

use super::*;
use crate::log::{Resumed, WireDialect};

/// A reply the upstream cut off says why, an empty one says it was asked again, and a
/// restart and a deliberate archive are each named where they happened.
#[test]
fn unfinished_replies_a_restart_and_an_archive_are_one_line_each() {
    let mut log = Fixture::new();
    log.add(Payload::record_only(
        AssistantOutput::empty(WireDialect::Messages),
        Exclusion::Upstream("upstream attempt failed (HTTP 529): overloaded".into()),
    ));
    log.add(Payload::record_only(
        AssistantOutput::empty(WireDialect::Messages),
        Exclusion::Empty,
    ));
    log.add(Payload::Resumed(Resumed::new(60_000)));
    log.add(Payload::archived(Some("web".into())));
    assert_eq!(
        whole(&log, &child(None)),
        "transcript of child, seq 0–3\n\
         0  a reply did not complete: upstream attempt failed (HTTP 529): overloaded\n\
         1  a reply came back with neither prose nor a call, and was asked again\n\
         2  resumed after its host restarted\n\
         3  archived\n"
    );
}
