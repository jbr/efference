//! The state topics the TUI reads (docs/tui.md, docs/control-api.md), held as the
//! `protocol` types the host serializes.
//!
//! A `state` frame carries one topic's whole value, `null` when it is clear, and the
//! store replaces that topic's slot with it: nothing is merged and nothing is copied
//! into a second type, so a field added to a topic's type is readable from its slot
//! with no line here. [`Topics`] has one slot per topic the TUI reads, [`TopicUpdate`] is
//! one decoded frame, and both come from the table below, so a topic the TUI starts to
//! read is one row.
//!
//! A frame is decoded by its topic's type. Its three outcomes:
//!
//! - a topic the TUI does not read is `Ok(None)` and is skipped, so a host that grows a
//!   topic stays usable;
//! - `null` clears the slot, as it clears the topic;
//! - a value that does not deserialize is an `Err`, and the caller drops the frame: the
//!   slot keeps the value it had.
//!
//! A slot that has never received a frame is `None` (empty for a list, idle for the
//! loop phase), which every reader treats as nothing to show.

use serde_json::Value;

use crate::api::{UpstreamFailure, UpstreamRetry};
use crate::policy::ReviewLevel;
use crate::protocol::state::ProcessState;
use crate::protocol::state::{
    AskView, Cold, CurrentOperations, Gate, LoopPhase, NewerVersion, PaceView, PendingProposal,
    RoleView, SessionIdentity, Stall, StartingView, UsageView, Verbs,
};

/// Declare the topics: for each row the [`TopicUpdate`] variant, the topic's wire name,
/// the [`Topics`] field, and the slot's type, which is also the type its frames
/// deserialize as.
macro_rules! topics {
    ($($variant:ident $name:literal $field:ident: $slot:ty,)*) => {
        /// The latest value of each state topic the TUI reads.
        #[derive(Debug, Default)]
        pub(crate) struct Topics {
            $(pub $field: $slot,)*
        }

        /// One topic's new value, decoded from a `state` frame.
        #[derive(Debug)]
        pub(crate) enum TopicUpdate {
            $($variant($slot),)*
        }

        impl TopicUpdate {
            /// The wire names of the topics the TUI reads, in table order.
            #[cfg(test)]
            pub(crate) const NAMES: &[&str] = &[$($name,)*];

            /// Decode the frame of topic `kind` carrying `data`.
            pub(crate) fn decode(
                kind: &str,
                data: Value,
            ) -> Result<Option<Self>, serde_json::Error> {
                Ok(Some(match kind {
                    $($name => Self::$variant(serde_json::from_value(data)?),)*
                    _ => return Ok(None),
                }))
            }
        }

        #[cfg(test)]
        impl TopicUpdate {
            /// The value the update carries, serialized again: what a lossless read gives
            /// back.
            pub(crate) fn carried(&self) -> Value {
                match self {
                    $(Self::$variant(value) => {
                        serde_json::to_value(value).unwrap_or(Value::Null)
                    },)*
                }
            }
        }

        impl Topics {
            /// Replace the slot `update` names with the value it carries.
            pub(crate) fn replace(&mut self, update: TopicUpdate) {
                match update {
                    $(TopicUpdate::$variant(value) => self.$field = value,)*
                }
            }
        }
    };
}

topics! {
    Gate "gate" gate: Option<Gate>,
    Ask "ask" ask: Option<AskView>,
    Processes "processes" processes: Vec<ProcessState>,
    Usage "usage" usage: Option<UsageView>,
    Role "role" role: Option<RoleView>,
    Proposals "proposals" proposals: Vec<PendingProposal>,
    Pace "pace" pace: Option<PaceView>,
    ReviewLevel "review_level" review_level: Option<ReviewLevel>,
    ResolvedReviewLevel "resolved_review_level" resolved_review_level: Option<ReviewLevel>,
    LoopPhase "loop_phase" loop_phase: LoopPhase,
    Verbs "verbs" verbs: Option<Verbs>,
    Stall "stall" stall: Option<Stall>,
    UpstreamFailure "upstream_failure" upstream_failure: Option<UpstreamFailure>,
    UpstreamRetry "upstream_retry" upstream_retry: Option<UpstreamRetry>,
    Cold "cold" cold: Option<Cold>,
    SessionIdentity "session_identity" session_identity: Option<SessionIdentity>,
    CurrentOperations "current_operations" current_operations: Option<CurrentOperations>,
    Starting "starting" starting: Option<StartingView>,
    NewerVersion "newer_version" newer_version: Option<NewerVersion>,
}

#[cfg(test)]
mod test;
