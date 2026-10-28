//! Bridging the daemon's unified control stream into typed [`TuiEvent`]s
//! (docs/control-api.md).

use super::*;

/// Decode the unified control stream into [`TuiEvent`]s. One connection, all three
/// durability classes — `log` frames become [`TuiEvent::Entry`], `output` frames the
/// live-tail deltas, and `state` frames the sidebar/status chrome. The stream delivers
/// a snapshot of every topic on connect and a whole-topic replacement on each change,
/// coherent with the log frames around it, so there is nothing to poll.
pub(super) async fn bridge(
    client: Client,
    mut events: trillium_client::sse::EventStream,
    tx: Sender<TuiEvent>,
) -> Result<()> {
    // The three policy-shaped topics are merged into one PolicyView for the sidebar's
    // convenience (docs/permissions.md), so the bridge holds the latest value of each
    // and re-merges when any of them moves.
    let mut policy = PolicyTopics::default();
    // Panel rows whose retained output has been seeded. The `output` class is lossy
    // with no replay, so a process already running at connect has no deltas to build
    // a tail from — `GET /processes/:id/output` is the seed (docs/control-api.md).
    // A row is seeded when it first becomes panel-visible, which also covers a
    // foreground command flipping to background mid-run.
    let mut seeded: std::collections::HashSet<u64> = std::collections::HashSet::new();

    loop {
        match events.next().await {
            Some(Ok(event)) => {
                let fresh = unseeded_panel_rows(&event, &mut seeded);
                if decode_frame(&event, &mut policy, &tx).is_break() {
                    break; // UI thread gone.
                }
                // After the topic frame (so the row exists to append onto), fetch each
                // fresh row's retained output and deliver it as an ordinary tail delta.
                for id in fresh {
                    let Ok(body) = client.process_output(id, None).await else {
                        continue;
                    };
                    let chunk = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v["output"].as_str().map(str::to_string))
                        .filter(|chunk| !chunk.is_empty());
                    if let Some(chunk) = chunk
                        && tx.send(TuiEvent::ProcessOutput { id, chunk }).is_err()
                    {
                        return Ok(()); // UI thread gone.
                    }
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

/// The ids a `processes` topic frame makes newly panel-visible (background,
/// un-muted) that have not been seeded yet — marking them seeded as it collects.
/// Any other frame yields nothing.
fn unseeded_panel_rows(
    event: &trillium_client::sse::Event,
    seeded: &mut std::collections::HashSet<u64>,
) -> Vec<u64> {
    let Ok(frame) = serde_json::from_str::<serde_json::Value>(event.data()) else {
        return Vec::new();
    };
    if frame["class"] != "state" || frame["kind"] != "processes" {
        return Vec::new();
    }
    frame["data"]
        .as_array()
        .map(|procs| {
            procs
                .iter()
                .filter(|p| p["background"] == true && p["muted"] == false)
                .filter_map(|p| p["id"].as_u64())
                .filter(|id| seeded.insert(*id))
                .collect()
        })
        .unwrap_or_default()
}

/// The latest value of each topic that feeds [`PolicyView`]. `role` starts absent —
/// no view is emitted before it arrives (the connect snapshot delivers it, so this is
/// a moment, not a mode).
#[derive(Default)]
struct PolicyTopics {
    role: Option<serde_json::Value>,
    proposals: serde_json::Value,
    review_level: serde_json::Value,
    resolved_review_level: serde_json::Value,
}

impl PolicyTopics {
    fn view(&self) -> Option<PolicyView> {
        let role = self.role.as_ref()?;
        parse::parse_policy(
            role,
            &self.proposals,
            &self.review_level,
            &self.resolved_review_level,
        )
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
        Some("log") => {
            let Ok(entry) = serde_json::from_value::<LogEntry>(data.clone()) else {
                return Continue(());
            };
            // The view's artifact links ride beside the entry, not in it: they are
            // derived on every read, never logged (docs/artifacts.md).
            let links = super::artifacts::external_links(data);
            if tx.send(TuiEvent::Entry(entry)).is_err() {
                return Break(());
            }
            for (coordinate, url) in links {
                if tx.send(TuiEvent::ArtifactUrl { coordinate, url }).is_err() {
                    return Break(());
                }
            }
            return Continue(());
        }
        Some("output") => return decode_delta(kind, data, tx),
        Some("state") => match kind {
            "gate" => Some(send(TuiEvent::Gate(parse::parse_pending_gate(data)))),
            "ask" => Some(send(TuiEvent::Ask(parse::parse_ask(data)))),
            "processes" => Some(send(TuiEvent::Processes(parse::parse_processes(data)))),
            // A null `usage` topic (no requests yet) has nothing to render; the status
            // bar keeps its absent state.
            "usage" => parse::parse_usage(data).map(|usage| send(TuiEvent::Usage(usage))),
            "upstream_failure" => Some(send(TuiEvent::UpstreamFailure(
                parse::parse_upstream_failure(data),
            ))),
            // Level-triggered: a `null` frame is the all-clear, so it is forwarded
            // as `None` rather than skipped (docs/transport.md).
            "upstream_retry" => Some(send(TuiEvent::UpstreamRetry(parse::parse_upstream_retry(
                data,
            )))),
            // Level-triggered like the retry above: a `null` frame means a
            // trajectory is running again, so it is forwarded as `None`
            // (docs/transport.md).
            "stall" => Some(send(TuiEvent::Stall(parse::parse_stall(data)))),
            // Level-triggered: `null` clears the header's expired-cache segment.
            "cold" => Some(send(TuiEvent::CacheExpired(parse::parse_cold(data)))),
            // Level-triggered: `null` while no newer release is known.
            "newer_version" => Some(send(TuiEvent::NewerVersion(
                data["version"].as_str().map(str::to_string),
            ))),
            // Static per session today, so this arrives once on the connect
            // snapshot; forwarded through the same path as anything else that
            // could move (docs/control-api.md).
            "session_identity" => Some(send(TuiEvent::SessionIdentity(
                parse::parse_session_identity(data),
            ))),
            // Level-triggered: `null` means nothing is open.
            "current_operations" => Some(send(TuiEvent::Operations(
                super::operations::parse_current_operations(data),
            ))),
            "pace" => parse::parse_pace(data).map(|pace| send(TuiEvent::Pace(pace))),
            "loop_phase" => serde_json::from_value(data.clone())
                .ok()
                .map(|phase| send(TuiEvent::Phase(phase))),
            "verbs" => Some(send(TuiEvent::Verbs(super::VerbsView::parse(data)))),
            // Level-triggered: `null` once the session has started.
            "starting" => Some(send(TuiEvent::Starting(
                super::starting::StartingView::parse(data),
            ))),
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
            "resolved_review_level" => {
                policy.resolved_review_level = data.clone();
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
        // `index` and `call_id` are what scope a delta to the block it extends
        // (docs/control-api.md); a frame missing either is dropped rather than
        // guessed at, since attributing a fragment to the wrong call is the defect
        // the scoping exists to prevent.
        "tool_call_start" => match (
            value["index"].as_u64(),
            value["call_id"].as_str(),
            value["name"].as_str(),
        ) {
            (Some(index), Some(call_id), Some(name)) => Some(
                tx.send(TuiEvent::ToolCallStart {
                    index: index as usize,
                    call_id: call_id.to_string(),
                    name: name.to_string(),
                })
                .map_err(Box::new),
            ),
            _ => None,
        },
        "tool_call_input" => match (value["index"].as_u64(), value["fragment"].as_str()) {
            (Some(index), Some(fragment)) => Some(
                tx.send(TuiEvent::ToolCallInput {
                    index: index as usize,
                    fragment: fragment.to_string(),
                })
                .map_err(Box::new),
            ),
            _ => None,
        },
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
        "process_output" => match (value["id"].as_u64(), value["chunk"].as_str()) {
            (Some(id), Some(chunk)) => Some(
                tx.send(TuiEvent::ProcessOutput {
                    id,
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
