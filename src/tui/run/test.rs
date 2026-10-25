use super::*;

#[tokio::test(start_paused = true)]
async fn unanswered_artifact_lookup_times_out() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let mut started = Some(started_tx);
    let lookup = artifact_resolution(std::future::poll_fn(move |_| {
        if let Some(started) = started.take() {
            let _ = started.send(());
        }
        std::task::Poll::Pending
    }));
    tokio::pin!(lookup);

    tokio::select! {
        result = &mut lookup => panic!("pending lookup completed unexpectedly: {result:?}"),
        _ = started_rx => {}
    }
    tokio::time::advance(ARTIFACT_RESOLUTION_TIMEOUT).await;

    assert!(lookup.await.is_none());
}
