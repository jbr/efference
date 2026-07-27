//! Bridging the daemon's two SSE streams into typed [`TuiEvent`]s (docs/control-channel.md).

use super::*;

/// Decode the daemon's two SSE streams into [`TuiEvent`]s. Mirrors the decode in
/// `main.rs`'s `render_streams`, but emits typed events instead of printing.
pub(super) async fn bridge(client: Client, tx: Sender<TuiEvent>) -> Result<()> {
    let mut events = client.events(Some(0)).await?;
    let mut deltas = client.stream().await?;
    let mut deltas_done = false;

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(event)) => {
                    if event.event_type() != Some("keepalive")
                        && let Ok(entry) = serde_json::from_str::<LogEntry>(event.data())
                        && tx.send(TuiEvent::Entry(entry)).is_err()
                    {
                        break; // UI thread gone.
                    }
                }
                // Stream error or end: the durable tail is authoritative, so its
                // loss means the daemon is gone.
                _ => {
                    let _ = tx.send(TuiEvent::Disconnected);
                    break;
                }
            },
            delta = deltas.next(), if !deltas_done => match delta {
                Some(Ok(delta)) => {
                    if decode_delta(&delta, &tx).is_break() {
                        break;
                    }
                }
                Some(Err(_)) | None => deltas_done = true,
            },
        }
    }
    Ok(())
}

/// Decode one ephemeral delta event, sending the matching [`TuiEvent`].
pub(super) fn decode_delta(
    event: &trillium_client::sse::Event,
    tx: &Sender<TuiEvent>,
) -> std::ops::ControlFlow<()> {
    let value: serde_json::Value = serde_json::from_str(event.data()).unwrap_or_default();
    // A failed send carries the whole rejected `TuiEvent` back in `SendError`,
    // which makes the `Err` variant large; we only care *that* it failed, so box
    // it to keep the result small (clippy `result_large_err`).
    let sent = match event.event_type() {
        Some("assistant_delta") => value["text"].as_str().map(|text| {
            tx.send(TuiEvent::AssistantDelta(text.to_string()))
                .map_err(Box::new)
        }),
        Some("thinking_delta") => value["text"].as_str().map(|text| {
            tx.send(TuiEvent::ThinkingDelta(text.to_string()))
                .map_err(Box::new)
        }),
        Some("tool_call_start") => value["name"].as_str().map(|name| {
            tx.send(TuiEvent::ToolCallStart {
                name: name.to_string(),
            })
            .map_err(Box::new)
        }),
        Some("tool_call_input") => value["fragment"].as_str().map(|fragment| {
            tx.send(TuiEvent::ToolCallInput {
                fragment: fragment.to_string(),
            })
            .map_err(Box::new)
        }),
        Some("tool_output") => match (value["call_id"].as_str(), value["chunk"].as_str()) {
            (Some(call_id), Some(chunk)) => Some(
                tx.send(TuiEvent::ToolOutput {
                    call_id: call_id.to_string(),
                    chunk: chunk.to_string(),
                })
                .map_err(Box::new),
            ),
            _ => None,
        },
        Some("bg_output") => match (value["handle"].as_str(), value["chunk"].as_str()) {
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
