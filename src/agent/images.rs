//! The loop's side of an image refusal (docs/transport.md): the client learns it
//! and withholds images from its requests; the loop closes the tool layer's image
//! gate to match and records the lesson, and a resumed session reads that record
//! back.

use super::*;

/// Restore a refusal recorded before a restart, so the resumed session withholds
/// images from its first request instead of paying for the lesson again.
pub(super) fn restore_image_refusal(shared: &Session, client: &Client, toolbox: &mut Toolbox) {
    let recorded = shared
        .snapshot()
        .into_iter()
        .find_map(|entry| match entry.payload {
            Payload::ImagesRefused { refusal } => Some(refusal),
            _ => None,
        });
    if let Some(refusal) = recorded {
        client.refuse_images(refusal);
        toolbox.refuse_images();
    }
}

/// When the client has learned a refusal the toolbox has not, close the gate and
/// append the record. Checked after every main-loop request, so a lesson a clone
/// learned (the shared fork peek, the keepalive) is recorded at the next one.
pub(super) fn record_image_refusal(
    shared: &Session,
    client: &Client,
    toolbox: &mut Toolbox,
    turn: Turn,
) -> std::io::Result<()> {
    let Some(refusal) = client.images_refused() else {
        return Ok(());
    };
    if !toolbox.image_input() {
        return Ok(());
    }
    toolbox.refuse_images();
    eprintln!("[images withheld from here on: the endpoint refused them]");
    shared.append(
        turn,
        Payload::ImagesRefused {
            refusal: refusal.to_string(),
        },
    )?;
    Ok(())
}
