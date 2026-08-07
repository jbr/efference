//! Bridging the daemon's unified control stream into typed [`TuiEvent`]s
//! (docs/control-api.md).

use super::*;

/// Decode the unified control stream into [`TuiEvent`]s. One connection, all three
/// durability classes — `log` frames become [`TuiEvent::Entry`], `output` frames the
/// live-tail deltas, and `state` frames the sidebar/status chrome that a 400 ms
/// `GET /state` poll used to feed. The stream delivers a snapshot of every topic on
/// connect and a whole-topic replacement on each change, coherent with the log frames
/// around it, so there is nothing left to poll.
pub(super) async fn bridge(client: Client, tx: Sender<TuiEvent>) -> Result<()> {
    let mut events = client.events(Some(0), Some("log,output,state")).await?;
    // The three policy-shaped topics are merged into one PolicyView for the sidebar's
    // convenience (docs/permissions.md), so the bridge holds the latest value of each
    // and re-merges when any of them moves.
    let mut policy = PolicyTopics::default();

    loop {
        match events.next().await {
            Some(Ok(event)) => {
                if decode_frame(&event, &mut policy, &tx).is_break() {
                    break; // UI thread gone.
                }
            }
            // Stream error or end: the durable tail is authoritative, so its
            // loss means the daemon is gone.
            _ => {
                let _ = tx.send(TuiEvent::Disconnected);
                break;
            }
        }
    }
    Ok(())
}

/// The latest value of each topic that feeds [`PolicyView`]. `role` starts absent —
/// no view is emitted before it arrives (the connect snapshot delivers it, so this is
/// a moment, not a mode).
#[derive(Default)]
struct PolicyTopics {
    role: Option<serde_json::Value>,
    proposals: serde_json::Value,
    review_level: serde_json::Value,
}

impl PolicyTopics {
    fn view(&self) -> Option<PolicyView> {
        let role = self.role.as_ref()?;
        parse::parse_policy(role, &self.proposals, &self.review_level)
    }
}

/// Decode one frame of the unified stream, sending the matching [`TuiEvent`]s.
///
/// An unparseable frame, an unknown class, and an unknown state topic are all skipped
/// rather than fatal, so a daemon that grows a class or topic this client doesn't know
/// stays usable.
fn decode_frame(
    event: &trillium_client::sse::Event,
    policy: &mut PolicyTopics,
    tx: &Sender<TuiEvent>,
) -> std::ops::ControlFlow<()> {
    use std::ops::ControlFlow::{Break, Continue};

    let Ok(frame) = serde_json::from_str::<serde_json::Value>(event.data()) else {
        return Continue(());
    };
    let kind = frame["kind"].as_str().unwrap_or_default();
    let data = &frame["data"];
    // Boxed for the same reason as in `decode_delta`: a failed send carries the whole
    // rejected `TuiEvent` back, and we only care *that* it failed.
    let send = |event: TuiEvent| tx.send(event).map_err(Box::new);

    let sent = match frame["class"].as_str() {
        Some("log") => serde_json::from_value::<LogEntry>(data.clone())
            .ok()
            .map(|entry| send(TuiEvent::Entry(entry))),
        Some("output") => return decode_delta(kind, data, tx),
        Some("state") => match kind {
            "gate" => Some(send(TuiEvent::Gate(parse::parse_pending_gate(data)))),
            "processes" => Some(send(TuiEvent::Processes(parse::parse_processes(data)))),
            // A null `usage` topic (no requests yet) has nothing to render — as under
            // the poll, the status bar keeps its absent state.
            "usage" => parse::parse_usage(data).map(|usage| send(TuiEvent::Usage(usage))),
            "upstream_failure" => Some(send(TuiEvent::UpstreamFailure(
                parse::parse_upstream_failure(data),
            ))),
            "pace" => parse::parse_pace(data).map(|pace| send(TuiEvent::Pace(pace))),
            "running" => data.as_bool().map(|r| send(TuiEvent::Running(r))),
            // The three policy-shaped topics update the held copy, then re-emit the
            // merged view. The connect snapshot delivers them in topic order (role
            // first), so the first emission already has a role to merge around.
            "role" => {
                policy.role = Some(data.clone());
                policy.view().map(|v| send(TuiEvent::Policy(v)))
            }
            "proposals" => {
                policy.proposals = data.clone();
                policy.view().map(|v| send(TuiEvent::Policy(v)))
            }
            "review_level" => {
                policy.review_level = data.clone();
                policy.view().map(|v| send(TuiEvent::Policy(v)))
            }
            _ => None,
        },
        _ => None,
    };
    // `Some(Err(..))` means the UI receiver was dropped → stop.
    if matches!(sent, Some(Err(_))) {
        Break(())
    } else {
        Continue(())
    }
}

/// Decode one `output`-class frame's payload, sending the matching [`TuiEvent`].
fn decode_delta(
    kind: &str,
    value: &serde_json::Value,
    tx: &Sender<TuiEvent>,
) -> std::ops::ControlFlow<()> {
    // A failed send carries the whole rejected `TuiEvent` back in `SendError`,
    // which makes the `Err` variant large; we only care *that* it failed, so box
    // it to keep the result small (clippy `result_large_err`).
    let sent = match kind {
        "assistant_delta" => value["text"].as_str().map(|text| {
            tx.send(TuiEvent::AssistantDelta(text.to_string()))
                .map_err(Box::new)
        }),
        "thinking_delta" => value["text"].as_str().map(|text| {
            tx.send(TuiEvent::ThinkingDelta(text.to_string()))
                .map_err(Box::new)
        }),
        "tool_call_start" => value["name"].as_str().map(|name| {
            tx.send(TuiEvent::ToolCallStart {
                name: name.to_string(),
            })
            .map_err(Box::new)
        }),
        "tool_call_input" => value["fragment"].as_str().map(|fragment| {
            tx.send(TuiEvent::ToolCallInput {
                fragment: fragment.to_string(),
            })
            .map_err(Box::new)
        }),
        "tool_output" => match (value["call_id"].as_str(), value["chunk"].as_str()) {
            (Some(call_id), Some(chunk)) => Some(
                tx.send(TuiEvent::ToolOutput {
                    call_id: call_id.to_string(),
                    chunk: chunk.to_string(),
                })
                .map_err(Box::new),
            ),
            _ => None,
        },
        "bg_output" => match (value["handle"].as_str(), value["chunk"].as_str()) {
            (Some(handle), Some(chunk)) => Some(
                tx.send(TuiEvent::BgOutput {
                    handle: handle.to_string(),
                    chunk: chunk.to_string(),
                })
                .map_err(Box::new),
            ),
            _ => None,
        },
        _ => None,
    };
    // `Some(Err(..))` means the UI receiver was dropped → stop.
    if matches!(sent, Some(Err(_))) {
        std::ops::ControlFlow::Break(())
    } else {
        std::ops::ControlFlow::Continue(())
    }
}
