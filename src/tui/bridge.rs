//! Bridging the daemon's unified control stream into typed [`TuiEvent`]s
//! (docs/control-api.md).

use std::ops::ControlFlow::{self, Break, Continue};

use super::topics::TopicUpdate;
use super::*;

/// Decode the unified control stream into [`TuiEvent`]s. One connection, all three
/// durability classes — `log` frames become [`TuiEvent::Entry`], `output` frames the
/// live-tail deltas, and `state` frames a [`TuiEvent::Topic`] each. The stream delivers
/// a snapshot of every topic on connect and a whole-topic replacement on each change,
/// coherent with the log frames around it, so there is nothing to poll.
pub(super) async fn bridge(
    client: Client,
    mut events: trillium_client::sse::EventStream,
    tx: Sender<TuiEvent>,
) -> Result<()> {
    // Panel rows whose retained output has been seeded. The `output` class is lossy
    // with no replay, so a process already running at connect has no deltas to build
    // a tail from — `GET /processes/:id/output` is the seed (docs/control-api.md).
    // A row is seeded when it first becomes panel-visible, which also covers a
    // foreground command flipping to background mid-run.
    let mut seeded: std::collections::HashSet<u64> = std::collections::HashSet::new();

    loop {
        match events.next().await {
            Some(Ok(event)) => {
                let Continue(fresh) = decode_frame(event.data(), &mut seeded, &tx) else {
                    break; // UI thread gone.
                };
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

/// Decode one frame of the unified stream, sending the matching [`TuiEvent`]s. Continues
/// with the ids of the process rows the frame made newly panel-visible and `seeded` did
/// not hold yet, marking them seeded as it collects; breaks when the UI thread is gone.
///
/// An unparseable frame, an unknown class, an unknown state topic, and a topic value that
/// does not deserialize are all skipped rather than fatal, so a daemon that grows a class
/// or topic this client doesn't know stays usable (the slot keeps the value it had).
fn decode_frame(
    data: &str,
    seeded: &mut std::collections::HashSet<u64>,
    tx: &Sender<TuiEvent>,
) -> ControlFlow<(), Vec<u64>> {
    let Ok(mut frame) = serde_json::from_str::<serde_json::Value>(data) else {
        return Continue(Vec::new());
    };
    let class = frame["class"].as_str().unwrap_or_default().to_string();
    let kind = frame["kind"].as_str().unwrap_or_default().to_string();
    let data = frame["data"].take();

    match class.as_str() {
        "log" => {
            let Ok(entry) = serde_json::from_value::<LogEntry>(data.clone()) else {
                return Continue(Vec::new());
            };
            // The view's artifact links ride beside the entry, not in it: they are
            // derived on every read, never logged (docs/artifacts.md).
            let links = artifacts::external_links(&data);
            if tx.send(TuiEvent::Entry(entry)).is_err() {
                return Break(());
            }
            for (coordinate, url) in links {
                if tx.send(TuiEvent::ArtifactUrl { coordinate, url }).is_err() {
                    return Break(());
                }
            }
            Continue(Vec::new())
        }
        "output" => decode_delta(&kind, &data, tx).map_continue(|()| Vec::new()),
        "state" => {
            let Ok(Some(update)) = TopicUpdate::decode(&kind, data) else {
                return Continue(Vec::new());
            };
            let fresh = match &update {
                TopicUpdate::Processes(processes) => processes
                    .iter()
                    .filter(|process| process_panel::listed(process))
                    .map(|process| process.id)
                    .filter(|id| seeded.insert(*id))
                    .collect(),
                _ => Vec::new(),
            };
            if tx.send(TuiEvent::Topic(update)).is_err() {
                return Break(());
            }
            Continue(fresh)
        }
        _ => Continue(Vec::new()),
    }
}

/// Decode one `output`-class frame's payload, sending the matching [`TuiEvent`].
fn decode_delta(kind: &str, value: &serde_json::Value, tx: &Sender<TuiEvent>) -> ControlFlow<()> {
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
        Break(())
    } else {
        Continue(())
    }
}

#[cfg(test)]
mod test;
