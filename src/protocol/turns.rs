//! A turn submitted over the control channel (docs/control-api.md): the id that names
//! it, where it went, and how a turn still waiting in the queue reads.

use serde::{Deserialize, Serialize};

use crate::log::attachment::Arrival;

/// Mint a submission id for a `POST /v1/turns` whose body didn't carry one, so
/// every submitted turn is identified in the log regardless of client
/// (docs/control-api.md). Public because a client that wants its submission
/// addressable — for the delivery echo, or to recall it — mints its own with
/// the same scheme rather than inventing a second one (docs/tui.md).
pub fn mint_submission_id() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where a submitted turn went: the `delivery` a `POST /turns` answer carries
/// (docs/control-api.md), and the one the `verbs` topic says a turn would get now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Landing {
    /// It answered the question it named.
    Answered,
    /// The loop takes it at once.
    Now,
    /// It waits in the queue for the loop's next juncture.
    Queued,
    /// An agent's turn, queued behind the operator's pace hold until it is released
    /// (docs/control-channel.md).
    Held,
}

impl Landing {
    /// The `delivery` a `POST /turns` answer carries (docs/control-api.md).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Now => "now",
            Self::Queued => "queued",
            Self::Held => "held",
        }
    }

    /// The landing a `delivery` string names.
    pub fn parse(delivery: &str) -> Option<Self> {
        match delivery {
            "answered" => Some(Self::Answered),
            "now" => Some(Self::Now),
            "queued" => Some(Self::Queued),
            "held" => Some(Self::Held),
            _ => None,
        }
    }
}

/// A turn submitted and not yet taken by the loop, as the `queued` topic lists it
/// (docs/control-api.md). The same shape names the turns a recall or a question
/// withdrew and a failed start handed back, so a client puts each back in its composer.
/// A pending turn is never logged: this is the only place it is readable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedTurn {
    /// The id the turn was submitted under, which a recall addresses.
    pub submission_id: String,
    pub text: String,
    /// The files its composer attached; absent when there are none, so a text-only turn
    /// reads as it always has.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Arrival>,
}

#[cfg(test)]
mod test;
