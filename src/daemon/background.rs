//! The detached drain task for one backgrounded process (docs/supervisor.md).

use super::*;

/// The detached drain task for one backgrounded process (docs/supervisor.md): feed every
/// output chunk into the registry's capture buffer until the child exits (EOF —
/// reached naturally, or because a kill closed the pipes), then mark it exited and
/// fire its exit notification into the inbox, where the next triage gate adjudicates
/// it like any notification. The supervisor lock is taken per chunk and never held
/// across an await.
pub(crate) async fn drain_background(
    shared: Arc<Shared>,
    handle: String,
    mut child: tokio::process::Child,
    mut stdout: tokio::process::ChildStdout,
    mut stderr: tokio::process::ChildStderr,
) {
    crate::bash::drain_pipes(
        &mut stdout,
        &mut stderr,
        std::future::pending::<()>(),
        |chunk| {
            shared.supervisor.lock().unwrap().push_chunk(&handle, chunk);
            // Smooth the live process panel between `/state` polls (TUI-bg-2). Lossy
            // and best-effort — the poll snapshot remains the authoritative tail.
            shared.publish_delta(StreamDelta::BgOutput {
                handle: handle.clone(),
                chunk: String::from_utf8_lossy(chunk).into_owned(),
            });
        },
    )
    .await;
    let status = child.wait().await.ok();
    let summary = shared.supervisor.lock().unwrap().finish(&handle, status);
    if let Some((source, body)) = summary {
        shared.notify(source, body, Some(handle), None);
    }
}
