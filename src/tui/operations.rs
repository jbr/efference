//! The status bar's account of what the loop is waiting on, from the
//! `current_operations` topic (docs/tui.md, docs/control-api.md).
//!
//! The topic carries absolute instants and no ticking field, so the elapsed time is
//! computed here from the client's clock; `refresh_clocks` re-derives the text each
//! second, which keeps rendering a pure function of state. The wording matches the
//! web client's turn state (docs/web-client.md).

use serde_json::Value;

/// One open upstream request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenRequestView {
    /// `turn`, `peek`, `spot`, … — the topic's `origin`.
    pub origin: String,
    /// `sent`, `retrying`, or `streaming`.
    pub stage: String,
    /// Unix millis at which this stage began.
    pub since: u64,
    pub attempt: Option<u64>,
    pub status: Option<u64>,
}

/// One tool call executing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunningToolView {
    pub tool: String,
    pub intent: Option<String>,
    /// The shell command of a `bash` call; it labels the tool in place of the intent.
    pub command: Option<String>,
    pub since: u64,
}

/// The `current_operations` topic's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationsView {
    pub requests: Vec<OpenRequestView>,
    pub tools: Vec<RunningToolView>,
}

/// Parse the topic; `null` (nothing open) is no view.
pub(super) fn parse_current_operations(value: &Value) -> Option<OperationsView> {
    if value.is_null() {
        return None;
    }
    let requests = value
        .get("requests")?
        .as_array()?
        .iter()
        .filter_map(|r| {
            Some(OpenRequestView {
                origin: r.get("origin")?.as_str()?.to_string(),
                stage: r.get("stage")?.as_str()?.to_string(),
                since: r.get("since")?.as_u64()?,
                attempt: r.get("attempt").and_then(Value::as_u64),
                status: r.get("status").and_then(Value::as_u64),
            })
        })
        .collect();
    let tools = value
        .get("tools")?
        .as_array()?
        .iter()
        .filter_map(|t| {
            Some(RunningToolView {
                tool: t.get("tool")?.as_str()?.to_string(),
                intent: t.get("intent").and_then(Value::as_str).map(str::to_string),
                command: t.get("command").and_then(Value::as_str).map(str::to_string),
                since: t.get("since")?.as_u64()?,
            })
        })
        .collect();
    Some(OperationsView { requests, tools })
}

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
fn rank(origin: &str) -> usize {
    [
        "turn",
        "peek",
        "spot",
        "triage",
        "narrate",
        "title",
        "triage_primer",
        "keepalive",
    ]
    .iter()
    .position(|o| *o == origin)
    .unwrap_or(8)
}

fn request_text(request: &OpenRequestView, model: &str, now: u64) -> String {
    let since = elapsed(now.saturating_sub(request.since));
    let retrying = format!(
        "retrying upstream · attempt {} ({}) · {since}",
        request.attempt.map_or("?".into(), |a| a.to_string()),
        request.status.map_or("?".into(), |s| s.to_string()),
    );
    match request.origin.as_str() {
        "peek" if request.stage == "retrying" => retrying,
        "peek" => format!(
            "triage peek · {}{since}",
            if request.stage == "sent" {
                "waiting "
            } else {
                "streaming "
            }
        ),
        "triage" => format!("idle triage · {since}"),
        "spot" => format!("spotter judging · {since}"),
        "title" => format!("writing a title · {since}"),
        "narrate" => format!("writing the summary · {since}"),
        "triage_primer" | "keepalive" => format!("warming the cache · {since}"),
        // A turn, an aborted attempt, or an origin this build does not know.
        _ => match request.stage.as_str() {
            "retrying" => retrying,
            "streaming" => format!("{model} streaming"),
            _ => format!("waiting on {model} · {since}"),
        },
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
pub(crate) fn operation_text(view: &OperationsView, model: &str, now: u64) -> Option<String> {
    let lead = view
        .requests
        .iter()
        .min_by_key(|r| (rank(&r.origin), r.since));
    // The turn's own request leads; any other yields to a running tool, which is
    // what the operator is waiting on when both are open.
    if let Some(lead) = lead.filter(|r| r.origin == "turn") {
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
