//! The tasks one attach runs beside the UI thread (docs/tui.md): the event bridge, and a
//! dispatcher per operator intent that turns what the UI sent into a control-channel
//! call and, where the answer matters, sends it home as a [`TuiEvent`].

use crossbeam_channel::Sender;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::{AbortHandle, JoinHandle};

use super::senders::{Receivers, Submission};
use super::*;
use crate::protocol::attention::AttentionReport;
use crate::protocol::turns::QueuedTurn;

/// The running tasks of one attach.
pub(super) struct Dispatchers {
    /// Everything that ends with the attach, in the order they are aborted.
    aborts: Vec<AbortHandle>,
    /// The attention reports' task, which gets a moment to send what the loop left
    /// queued before it is aborted with the rest.
    pub(super) attention: JoinHandle<()>,
}

impl Dispatchers {
    /// Spawn every task of an attach, each reading its far end of the channels the UI
    /// sends over, and the bridge reading `events`.
    pub(super) fn spawn(
        client: &Client,
        events: trillium_client::sse::EventStream,
        receivers: Receivers,
        tx: &Sender<TuiEvent>,
    ) -> Self {
        let Receivers {
            submit,
            recall,
            attachments,
            continue_loop,
            compaction,
            approve,
            pace,
            interrupt,
            sidebar,
            images,
            host,
            attention,
        } = receivers;

        // The bridge feeds the UI thread everything — log entries, live deltas, and the
        // state topics (gate/processes/usage/policy/pace/running) as level-triggered
        // frames on the same connection (docs/control-api.md). It ends when the stream
        // closes or the UI thread drops the receiver. No poll: the daemon pushes a
        // whole-topic replacement when a topic changes, and the connect snapshot seeds
        // every topic up front.
        let bridge = tokio::spawn(bridge(client.clone(), events, tx.clone()));
        let sender = turn_sender(client, submit, tx);
        let clipboard_watcher = clipboard_watcher(tx);
        let attacher = attachment_dispatcher(client, attachments, tx);
        let recaller = recall_dispatcher(client, recall, tx);
        let continuer = continue_sender(client, continue_loop, tx);
        let compactor = compaction_dispatcher(client, compaction, tx);
        let approver = approval_sender(client, approve);
        let pacer = pace_sender(client, pace);
        let interrupter = interrupt_sender(client, interrupt);
        let fetcher = image_fetcher(client, images, tx);
        let sidebar_dispatcher = sidebar_dispatcher(client, sidebar);
        // Host verbs (`host_dispatch.rs`): the routes addressed to the host rather
        // than to this session, so they run against a client re-based at the socket
        // root, where those routes live under either binding.
        let host_dispatcher = tokio::spawn(host_dispatch::serve(
            client.clone().with_prefix(""),
            host,
            tx.clone(),
        ));
        let attention = attention_task(client, attention);

        Self {
            aborts: vec![
                bridge.abort_handle(),
                sender.abort_handle(),
                recaller.abort_handle(),
                attacher.abort_handle(),
                clipboard_watcher.abort_handle(),
                continuer.abort_handle(),
                compactor.abort_handle(),
                approver.abort_handle(),
                pacer.abort_handle(),
                interrupter.abort_handle(),
                sidebar_dispatcher.abort_handle(),
                host_dispatcher.abort_handle(),
                fetcher.abort_handle(),
            ],
            attention,
        }
    }

    /// End every task but the attention reports', which the caller has already given
    /// its moment.
    pub(super) fn abort(self) {
        for task in self.aborts {
            task.abort();
        }
    }
}

/// Turn sender: each composer submission is POSTed as a user turn. A failure comes
/// home, since the pending echo alone would read as "still queued" forever: a
/// refusal over the pending question returns the turn to the composer, anything
/// else (an attachment no longer in the store) is a notice.
fn turn_sender(
    client: &Client,
    mut submit_rx: UnboundedReceiver<Submission>,
    events: &Sender<TuiEvent>,
) -> JoinHandle<()> {
    let turn_client = client.clone();
    let turn_events = events.clone();
    tokio::spawn(async move {
        while let Some(Submission { turn, answers }) = submit_rx.recv().await {
            let event = match turn_client.submit(&turn, answers.as_deref()).await {
                Ok(Ok(_)) => continue,
                Ok(Err(_refusal)) => TuiEvent::NotDelivered {
                    answered: answers.is_some(),
                    turn: QueuedTurn::from(&turn),
                },
                Err(e) => TuiEvent::Notice(format!("send failed: {e}")),
            };
            if turn_events.send(event).is_err() {
                break; // UI thread gone.
            }
        }
    })
}

/// Clipboard watch (docs/tui.md): a terminal's own paste carries no image, so the strip says
/// when `C-v` would fetch one. Learned from the pasteboard's change count and type list —
/// never its contents — polled because nothing announces a change.
fn clipboard_watcher(events: &Sender<TuiEvent>) -> JoinHandle<()> {
    let watch_events = events.clone();
    tokio::spawn(async move {
        if !pasteboard::Watch::SUPPORTED {
            return;
        }
        let mut watch = pasteboard::Watch::default();
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tick.tick().await;
            if let Some(found) = watch.poll()
                && watch_events.send(TuiEvent::ClipboardImage(found)).is_err()
            {
                break; // UI thread gone.
            }
        }
    })
}

/// Attachment dispatcher (docs/inbox.md): a staged file uploads at once and its answer
/// comes home keyed by the staging key; a removal is fire-and-forget.
fn attachment_dispatcher(
    client: &Client,
    mut attachments_rx: UnboundedReceiver<staged::AttachCmd>,
    events: &Sender<TuiEvent>,
) -> JoinHandle<()> {
    let attach_client = client.clone();
    let attach_events = events.clone();
    tokio::spawn(async move {
        while let Some(cmd) = attachments_rx.recv().await {
            match cmd {
                staged::AttachCmd::Upload { key, name, bytes } => {
                    // An image's thumbnail is decoded and scaled here, off the UI thread —
                    // a full-resolution screenshot is a noticeable decode.
                    if ::image::guess_format(&bytes).is_ok() {
                        let bytes = bytes.clone();
                        let events = attach_events.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Ok(image) = ::image::load_from_memory(&bytes) {
                                let image = image.thumbnail(512, 512);
                                let _ = events.send(TuiEvent::Thumbnail { key, image });
                            }
                        });
                    }
                    let result = attach_client
                        .upload_attachment(&name, bytes)
                        .await
                        .map_err(|e| e.to_string());
                    if attach_events
                        .send(TuiEvent::Uploaded { key, result })
                        .is_err()
                    {
                        break; // UI thread gone.
                    }
                }
                staged::AttachCmd::Remove { name } => {
                    let _ = attach_client.remove_attachment(&name).await;
                }
            }
        }
    })
}

/// Recall dispatcher (docs/tui.md): withdraw the pending-queue suffix from a submission id
/// and send home what the daemon *actually* popped. Like the continue sender this one answers
/// back, and the answer is the whole point — the composer splices the server's list, so a turn
/// the loop already dequeued is never handed back as if it were still a draft.
fn recall_dispatcher(
    client: &Client,
    mut recall_rx: UnboundedReceiver<String>,
    events: &Sender<TuiEvent>,
) -> JoinHandle<()> {
    let recall_client = client.clone();
    let recall_events = events.clone();
    tokio::spawn(async move {
        while let Some(submission_id) = recall_rx.recv().await {
            let event = match recall_client.withdraw_turns(&submission_id).await {
                Ok(withdrawn) => TuiEvent::Recalled(withdrawn),
                Err(e) => TuiEvent::Notice(format!("recall failed: {e}")),
            };
            if recall_events.send(event).is_err() {
                break; // UI thread gone.
            }
        }
    })
}

/// Continue sender: the empty-composer `⏎` hands control back with no input
/// (`POST /loop/continue`, docs/control-api.md). Unlike the other dispatchers
/// this one answers back: a decline is a normal outcome — the moment had no
/// continue in it — and the operator only learns which case from the daemon's
/// message, so it rides home as a `Notice` for the status line.
fn continue_sender(
    client: &Client,
    mut continue_rx: UnboundedReceiver<()>,
    events: &Sender<TuiEvent>,
) -> JoinHandle<()> {
    let continue_client = client.clone();
    let continue_events = events.clone();
    tokio::spawn(async move {
        while continue_rx.recv().await.is_some() {
            let event = match continue_client.continue_loop(None).await {
                Ok(crate::clients::client::ContinueOutcome::Accepted) => continue,
                Ok(crate::clients::client::ContinueOutcome::Declined { message }) => {
                    TuiEvent::Notice(message)
                }
                Err(e) => TuiEvent::Notice(format!("continue failed: {e}")),
            };
            if continue_events.send(event).is_err() {
                break; // UI thread gone.
            }
        }
    })
}

/// Compaction dispatcher (`POST /loop/compact`, docs/control-api.md): like the continue, a
/// decline is a normal outcome, and acceptance only means the summary is being
/// written — it arrives on the log when it lands.
fn compaction_dispatcher(
    client: &Client,
    mut compaction_rx: UnboundedReceiver<()>,
    events: &Sender<TuiEvent>,
) -> JoinHandle<()> {
    let compact_client = client.clone();
    let compaction_events = events.clone();
    tokio::spawn(async move {
        while compaction_rx.recv().await.is_some() {
            let note = match compact_client.compact().await {
                Ok(crate::clients::client::CompactOutcome::Accepted) => {
                    "compacting — the session's model is writing the summary".to_string()
                }
                Ok(crate::clients::client::CompactOutcome::Declined { message }) => message,
                Err(e) => format!("compaction failed: {e}"),
            };
            if compaction_events.send(TuiEvent::Notice(note)).is_err() {
                break; // UI thread gone.
            }
        }
    })
}

/// Approval sender: resolve a gate via `PUT /approvals/:call_id`.
fn approval_sender(
    client: &Client,
    mut approve_rx: UnboundedReceiver<(String, crate::record::log::Approval)>,
) -> JoinHandle<()> {
    let approve_client = client.clone();
    tokio::spawn(async move {
        while let Some((call_id, decision)) = approve_rx.recv().await {
            // The TUI is the operator's own client, so it names no decider.
            let _ = approve_client.approve(&call_id, &decision, None).await;
        }
    })
}

/// Pace sender: each pace key issues a `PATCH /loop {pace}` (docs/tui.md).
fn pace_sender(client: &Client, mut pace_rx: UnboundedReceiver<PaceCmd>) -> JoinHandle<()> {
    let pace_client = client.clone();
    tokio::spawn(async move {
        while let Some((speed_ms, held)) = pace_rx.recv().await {
            let _ = pace_client.pace(speed_ms, held).await;
        }
    })
}

/// Interrupt sender: the operator's intent over the in-flight tool. `Kill`
/// (the hard rung of graduated Esc, docs/tui.md) fires the loop interrupt;
/// `Detach` (the foreground→background handoff, docs/process-monitor.md) resolves
/// the foreground process and patches it background — the client verb owns
/// that resolution.
fn interrupt_sender(
    client: &Client,
    mut interrupt_rx: UnboundedReceiver<crate::tools::Interrupt>,
) -> JoinHandle<()> {
    let interrupt_client = client.clone();
    tokio::spawn(async move {
        while let Some(intent) = interrupt_rx.recv().await {
            match intent {
                crate::tools::Interrupt::Kill => {
                    let _ = interrupt_client
                        .interrupt(&crate::record::log::InterruptOrigin {
                            source: None,
                            control: Some("esc".into()),
                        })
                        .await;
                }
                crate::tools::Interrupt::Detach => {
                    let _ = interrupt_client.background(None).await;
                }
            }
        }
    })
}

/// Image fetcher (docs/tui.md): dereference a result image the UI thread saw
/// only as metadata — `GET /log/:seq/images/:idx` over the socket, decoded
/// here (off the UI thread), landed as a `TuiEvent::Image`. A failure lands
/// too: the reserved rows render the reason instead of pixels.
fn image_fetcher(
    client: &Client,
    mut images_rx: UnboundedReceiver<image::ImageKey>,
    events: &Sender<TuiEvent>,
) -> JoinHandle<()> {
    let image_client = client.clone();
    let image_events = events.clone();
    tokio::spawn(async move {
        while let Some((seq, idx)) = images_rx.recv().await {
            let image = match image_client.log_image(seq, idx).await {
                Ok(bytes) => ::image::load_from_memory(&bytes).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            if image_events
                .send(TuiEvent::Image { seq, idx, image })
                .is_err()
            {
                break; // UI thread gone.
            }
        }
    })
}

/// Sidebar verbs (docs/tui.md): the focused row's kill / commit / cancel,
/// each a direct control-channel call addressed by identity.
fn sidebar_dispatcher(
    client: &Client,
    mut sidebar_rx: UnboundedReceiver<SidebarCmd>,
) -> JoinHandle<()> {
    let sidebar_client = client.clone();
    tokio::spawn(async move {
        while let Some(cmd) = sidebar_rx.recv().await {
            match cmd {
                SidebarCmd::KillProcess(id) => {
                    let _ = sidebar_client.kill_process(id).await;
                }
                SidebarCmd::CommitProposal(seq) => {
                    let _ = sidebar_client
                        .commit_proposal(seq, "committed from the TUI")
                        .await;
                }
                SidebarCmd::CancelProposal(seq) => {
                    let _ = sidebar_client
                        .cancel_proposal(seq, "cancelled from the TUI")
                        .await;
                }
            }
        }
    })
}

/// Attention reports (`attention.rs`, docs/web-push.md): sent in order from a
/// task of their own, so a slow host never holds the UI loop. A failure is
/// logged and not shown; a `404` (a lone session bind has no attention table)
/// ends the task, which the UI loop notices as a closed channel.
fn attention_task(
    client: &Client,
    mut attention_rx: UnboundedReceiver<AttentionReport>,
) -> JoinHandle<()> {
    let attention_client = client.clone().with_prefix("");
    tokio::spawn(async move {
        while let Some(report) = attention_rx.recv().await {
            match attention_client.report_attention(&report).await {
                Ok(true) => {}
                Ok(false) => {
                    log::debug!("attention: the endpoint has no host table; reporting stops");
                    break;
                }
                Err(e) => log::warn!("attention report failed: {e}"),
            }
        }
    })
}
