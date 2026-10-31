//! The status bar's account of what the loop is waiting on, from the
//! `current_operations` topic (docs/tui.md, docs/control-api.md).
//!
//! The topic carries absolute instants and no ticking field, so the elapsed time is
//! computed here from the client's clock; `refresh_clocks` re-derives the text each
//! second, which keeps rendering a pure function of state. The wording matches the
//! web client's turn state (docs/web-client.md).

use crate::log::RequestOrigin;
use crate::protocol::state::{CurrentOperations, OpenRequest, OpenStage};

/// "45s", "2m 10s", "1h 05m".
fn elapsed(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m}m {:02}s", s % 60);
    }
    format!("{}h {:02}m", m / 60, m % 60)
}

/// Which request leads when several are open: the turn's own, then the side
/// requests by how much they hold the loop.
fn rank(origin: RequestOrigin) -> usize {
    match origin {
        RequestOrigin::Turn => 0,
        RequestOrigin::Triage => 1,
        RequestOrigin::Spot => 2,
        RequestOrigin::Compaction => 3,
        RequestOrigin::Title => 4,
        RequestOrigin::TriagePrimer => 5,
        RequestOrigin::Keepalive => 6,
        RequestOrigin::TriageKeepalive
        | RequestOrigin::Consolidation
        | RequestOrigin::AbortedAttempt
        | RequestOrigin::Unknown => 7,
    }
}

fn request_text(request: &OpenRequest, model: &str, now: u64) -> String {
    let since = elapsed(now.saturating_sub(request.since));
    let retrying = |attempt: u32, status: u16| {
        format!("retrying upstream · attempt {attempt} ({status}) · {since}")
    };
    match (request.origin, &request.stage) {
        (RequestOrigin::Triage, OpenStage::Retrying { attempt, status }) => {
            retrying(*attempt, *status)
        }
        (RequestOrigin::Triage, OpenStage::Sent) => format!("triage · waiting {since}"),
        (RequestOrigin::Triage, OpenStage::Streaming) => format!("triage · streaming {since}"),
        (RequestOrigin::Spot, _) => format!("spotter judging · {since}"),
        (RequestOrigin::Title, _) => format!("writing a title · {since}"),
        (RequestOrigin::Compaction, _) => format!("writing the summary · {since}"),
        (
            RequestOrigin::TriagePrimer | RequestOrigin::Keepalive | RequestOrigin::TriageKeepalive,
            _,
        ) => format!("warming the cache · {since}"),
        // A turn, an aborted attempt, or an origin this build does not know.
        (_, OpenStage::Retrying { attempt, status }) => retrying(*attempt, *status),
        (_, OpenStage::Streaming) => format!("{model} streaming"),
        (_, OpenStage::Sent) => format!("waiting on {model} · {since}"),
    }
}

/// What a pending gate or `ask` says in place of the operation text: the operator
/// is the one being waited on (docs/tui.md).
pub(crate) fn pending_text(gate: bool, ask: bool) -> Option<&'static str> {
    if gate {
        Some("waiting for your approval")
    } else if ask {
        Some("waiting for your answer")
    } else {
        None
    }
}

/// One line for the leading open operation, or `None` when none is open.
pub(crate) fn operation_text(view: &CurrentOperations, model: &str, now: u64) -> Option<String> {
    let lead = view
        .requests
        .iter()
        .min_by_key(|r| (rank(r.origin), r.since));
    // The turn's own request leads; any other yields to a running tool, which is
    // what the operator is waiting on when both are open.
    if let Some(lead) = lead.filter(|r| r.origin == RequestOrigin::Turn) {
        return Some(request_text(lead, model, now));
    }
    if let Some(tool) = view.tools.first() {
        // The command says more than the intent's sentence, in less room.
        let what = tool
            .command
            .as_deref()
            .or(tool.intent.as_deref())
            .unwrap_or("");
        let label: String = if what.chars().count() > 48 {
            format!("{}…", what.chars().take(47).collect::<String>())
        } else {
            what.to_string()
        };
        let colon = if label.is_empty() { "" } else { ": " };
        return Some(format!(
            "{}{colon}{label} · {}",
            tool.tool,
            elapsed(now.saturating_sub(tool.since))
        ));
    }
    lead.map(|r| request_text(r, model, now))
}

#[cfg(test)]
mod test;
