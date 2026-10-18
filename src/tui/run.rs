//! Entry points — attach to a daemon, or run the element gallery — and theme resolution.

use super::attention::Attender;
use super::senders::Submission;
use super::*;

/// Attach to a session and stay there until the operator leaves it.
///
/// A switch leaves by returning [`Exit::Switch`], and this is the loop that
/// honours it: the previous attach's tasks are already gone when it returns —
/// aborting the bridge drops the SSE stream, which closes the connection — so a
/// switch is a fresh handshake against the next session and never two cursors at
/// once (docs/control-api.md). `client` is re-based onto the new prefix over the
/// same transport, so the socket is dialled once per attach and nothing has to be
/// re-resolved.
pub async fn run(
    client: Client,
    theme: Option<String>,
    images: ImageMode,
    frame_cost: bool,
    mut attached: Attached,
) -> Result<()> {
    let mut client = client;
    loop {
        match attach(
            client.clone(),
            theme.clone(),
            images,
            frame_cost,
            attached.clone(),
        )
        .await?
        {
            Exit::Quit => return Ok(()),
            Exit::Switch { id } => {
                client = client.with_prefix(&format!("/sessions/{id}"));
                attached.draft = None;
            }
            Exit::SwitchWithDraft { id, draft } => {
                client = client.with_prefix(&format!("/sessions/{id}"));
                attached.draft = Some(draft);
            }
        }
    }
}

/// What this client is to the sessions it attaches, as opposed to how it draws them.
#[derive(Debug, Clone, Default)]
pub struct Attached {
    /// This client launched the host, and quitting it ends the host with every
    /// session on it (docs/operating.md): the island owner. `detach` would be
    /// false there, so the strip says `quit`.
    pub owns_host: bool,
    /// Text for the composer as the attach opens: the turns a failed start handed
    /// back, carried to the session its retry made (`starting.rs`).
    pub draft: Option<String>,
}

/// One attach: open the stream, run the UI, and tear every task down on the way
/// out. Returns why the loop ended.
async fn attach(
    client: Client,
    theme: Option<String>,
    images: ImageMode,
    frame_cost: bool,
    attached: Attached,
) -> Result<Exit> {
    // Validate the theme up front, before the alt-screen takes over, so a bad
    // name surfaces a clean error (with the list) instead of a silent fallback.
    let theme = resolve_theme(theme)?;
    // Open the event stream up front for the same reason: a socket nobody is
    // listening on, or a host prefix that names no session, is a plain error
    // here rather than an empty screen whose turns go nowhere.
    let events = client.events(Some(0), Some("log,output,state")).await?;

    let (tx, rx) = crossbeam_channel::unbounded::<TuiEvent>();
    let (senders, receivers) = Senders::channels();
    let Receivers {
        submit: mut submit_rx,
        recall: mut recall_rx,
        attachments: mut attachments_rx,
        continue_loop: mut continue_rx,
        compaction: mut compaction_rx,
        approve: mut approve_rx,
        pace: mut pace_rx,
        interrupt: mut interrupt_rx,
        sidebar: mut sidebar_rx,
        images: mut images_rx,
        host: host_rx,
        attention: mut attention_rx,
    } = receivers;

    // The bridge feeds the UI thread everything — log entries, live deltas, and the
    // state topics (gate/processes/usage/policy/pace/running) as level-triggered
    // frames on the same connection (docs/control-api.md). It ends when the stream
    // closes or the UI thread drops the receiver. No poll: the daemon pushes a
    // whole-topic replacement when a topic changes, and the connect snapshot seeds
    // every topic up front.
    let bridge = tokio::spawn(bridge(client.clone(), events, tx.clone()));

    // Turn sender: each composer submission is POSTed as a user turn. A failure comes
    // home, since the pending echo alone would read as "still queued" forever: a
    // refusal over the pending question returns the turn to the composer, anything
    // else (an attachment no longer in the store) is a notice.
    let turn_client = client.clone();
    let turn_events = tx.clone();
    let sender = tokio::spawn(async move {
        while let Some(Submission { turn, answers }) = submit_rx.recv().await {
            let event = match turn_client.submit(&turn, answers.as_deref()).await {
                Ok(Ok(_)) => continue,
                Ok(Err(_refusal)) => TuiEvent::NotDelivered {
                    answered: answers.is_some(),
                    turn: crate::client::WithdrawnTurn {
                        submission_id: turn.submission_id,
                        text: turn.text,
                        attachments: turn.attachments,
                    },
                },
                Err(e) => TuiEvent::Notice(format!("send failed: {e}")),
            };
            if turn_events.send(event).is_err() {
                break; // UI thread gone.
            }
        }
    });

    // Clipboard watch (docs/tui.md): a terminal's own paste carries no image, so the strip says
    // when `C-v` would fetch one. Learned from the pasteboard's change count and type list —
    // never its contents — polled because nothing announces a change.
    let watch_events = tx.clone();
    let clipboard_watcher = tokio::spawn(async move {
        if !crate::tui::pasteboard::Watch::SUPPORTED {
            return;
        }
        let mut watch = crate::tui::pasteboard::Watch::default();
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            tick.tick().await;
            if let Some(found) = watch.poll()
                && watch_events.send(TuiEvent::ClipboardImage(found)).is_err()
            {
                break; // UI thread gone.
            }
        }
    });

    // Attachment dispatcher (docs/inbox.md): a staged file uploads at once and its answer
    // comes home keyed by the staging key; a removal is fire-and-forget.
    let attach_client = client.clone();
    let attach_events = tx.clone();
    let attacher = tokio::spawn(async move {
        while let Some(cmd) = attachments_rx.recv().await {
            match cmd {
                crate::tui::staged::AttachCmd::Upload { key, name, bytes } => {
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
                crate::tui::staged::AttachCmd::Remove { name } => {
                    let _ = attach_client.remove_attachment(&name).await;
                }
            }
        }
    });

    // Recall dispatcher (docs/tui.md): withdraw the pending-queue suffix from a submission id
    // and send home what the daemon *actually* popped. Like the continue sender this one answers
    // back, and the answer is the whole point — the composer splices the server's list, so a turn
    // the loop already dequeued is never handed back as if it were still a draft.
    let recall_client = client.clone();
    let recall_events = tx.clone();
    let recaller = tokio::spawn(async move {
        while let Some(submission_id) = recall_rx.recv().await {
            let event = match recall_client.withdraw_turns(&submission_id).await {
                Ok(withdrawn) => TuiEvent::Recalled(withdrawn),
                Err(e) => TuiEvent::Notice(format!("recall failed: {e}")),
            };
            if recall_events.send(event).is_err() {
                break; // UI thread gone.
            }
        }
    });

    // Continue sender: the empty-composer `⏎` hands control back with no input
    // (`POST /loop/continue`, docs/control-api.md). Unlike the other dispatchers
    // this one answers back: a decline is a normal outcome — the moment had no
    // continue in it — and the operator only learns which case from the daemon's
    // message, so it rides home as a `Notice` for the status line.
    let continue_client = client.clone();
    let continue_events = tx.clone();
    let continuer = tokio::spawn(async move {
        while continue_rx.recv().await.is_some() {
            let event = match continue_client.continue_loop().await {
                Ok(crate::client::ContinueOutcome::Accepted) => continue,
                Ok(crate::client::ContinueOutcome::Declined { message }) => {
                    TuiEvent::Notice(message)
                }
                Err(e) => TuiEvent::Notice(format!("continue failed: {e}")),
            };
            if continue_events.send(event).is_err() {
                break; // UI thread gone.
            }
        }
    });

    // Compaction dispatcher (`POST /loop/compact`, docs/control-api.md): like the continue, a
    // decline is a normal outcome, and acceptance only means the summary is being
    // written — it arrives on the log when it lands.
    let compact_client = client.clone();
    let compaction_events = tx.clone();
    let compactor = tokio::spawn(async move {
        while compaction_rx.recv().await.is_some() {
            let note = match compact_client.compact().await {
                Ok(crate::client::CompactOutcome::Accepted) => {
                    "compacting — the session's model is writing the summary".to_string()
                }
                Ok(crate::client::CompactOutcome::Declined { message }) => message,
                Err(e) => format!("compaction failed: {e}"),
            };
            if compaction_events.send(TuiEvent::Notice(note)).is_err() {
                break; // UI thread gone.
            }
        }
    });

    // Approval sender: resolve a gate via `PUT /approvals/:call_id`.
    let approve_client = client.clone();
    let approver = tokio::spawn(async move {
        while let Some((call_id, decision)) = approve_rx.recv().await {
            // The TUI is the operator's own client, so it names no decider.
            let _ = approve_client.approve(&call_id, &decision, None).await;
        }
    });

    // Pace sender: each pace key issues a `PATCH /loop {pace}` (docs/tui.md).
    let pace_client = client.clone();
    let pacer = tokio::spawn(async move {
        while let Some((speed_ms, held)) = pace_rx.recv().await {
            let _ = pace_client.pace(speed_ms, held).await;
        }
    });

    // Interrupt sender: the operator's intent over the in-flight tool. `Kill`
    // (the hard rung of graduated Esc, docs/tui.md) fires the loop interrupt;
    // `Detach` (the foreground→background handoff, docs/process-monitor.md) resolves
    // the foreground process and patches it background — the client verb owns
    // that resolution.
    let interrupt_client = client.clone();
    let interrupter = tokio::spawn(async move {
        while let Some(intent) = interrupt_rx.recv().await {
            match intent {
                crate::frontend::Interrupt::Kill => {
                    let _ = interrupt_client
                        .interrupt(&crate::log::InterruptOrigin {
                            source: None,
                            control: Some("esc".into()),
                        })
                        .await;
                }
                crate::frontend::Interrupt::Detach => {
                    let _ = interrupt_client.background(None).await;
                }
            }
        }
    });

    // Image fetcher (docs/tui.md): dereference a result image the UI thread saw
    // only as metadata — `GET /log/:seq/images/:idx` over the socket, decoded
    // here (off the UI thread), landed as a `TuiEvent::Image`. A failure lands
    // too: the reserved rows render the reason instead of pixels.
    let image_client = client.clone();
    let image_events = tx.clone();
    let fetcher = tokio::spawn(async move {
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
    });

    // Sidebar verbs (docs/tui.md): the focused row's kill / commit / cancel,
    // each a direct control-channel call addressed by identity.
    let sidebar_client = client.clone();
    let sidebar_dispatcher = tokio::spawn(async move {
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
    });

    // Host verbs (`host_dispatch.rs`): the routes addressed to the host rather
    // than to this session, so they run against a client re-based at the socket
    // root, where those routes live under either binding.
    let host_dispatcher = tokio::spawn(super::host_dispatch::serve(
        client.clone().with_prefix(""),
        host_rx,
        tx.clone(),
    ));

    // Attention reports (`attention.rs`, docs/web-push.md): sent in order from a
    // task of their own, so a slow host never holds the UI loop. A failure is
    // logged and not shown; a `404` (a lone session bind has no attention table)
    // ends the task, which the UI loop notices as a closed channel.
    let attention_client = client.clone().with_prefix("");
    let attention_task = tokio::spawn(async move {
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
    });
    let attender = Attender::new(
        super::attention::process_client_id(),
        client.session_id().map(str::to_string),
        std::time::Instant::now(),
    );

    // The crossterm UI loop is blocking — run it off the async runtime.
    let ui = tokio::task::spawn_blocking(move || {
        ui_loop(
            rx,
            senders,
            theme,
            images,
            None,
            frame_cost,
            attached,
            Some(attender),
        )
    });

    let result = ui.await.context("the TUI thread panicked")?;
    // The loop's senders are gone, so the task ends once it has sent what the
    // loop left queued (the inactive report on quitting); bounded so a stalled
    // host cannot hold the process open.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), attention_task).await;
    bridge.abort();
    sender.abort();
    recaller.abort();
    attacher.abort();
    clipboard_watcher.abort();
    continuer.abort();
    compactor.abort();
    approver.abort();
    pacer.abort();
    interrupter.abort();
    sidebar_dispatcher.abort();
    host_dispatcher.abort();
    fetcher.abort();
    result.context("the TUI event loop failed")
}

/// Launch the TUI on the fixed [`gallery`] transcript — every element type, no
/// daemon. With `stream`, a scripted emitter also plays live tool-call events into
/// the session on a timer (`gallery/stream.rs`), which is how the animation-rate
/// render behaviours get driven without an upstream.
pub async fn run_gallery(
    theme: Option<String>,
    images: ImageMode,
    stream: Option<gallery::stream::Script>,
    frame_cost: bool,
) -> Result<()> {
    run_offline(
        gallery::log(),
        None,
        theme,
        images,
        stream,
        frame_cost,
        false,
    )
    .await
}

/// Launch the TUI **read-only** on a recorded session's log (`efference dev view`,
/// docs/tui.md): the same transcript rendering as a live attach, over entries
/// loaded from a jsonl file rather than a daemon's event stream. `label` names the
/// session in the status bar. Nothing here can write — the log is read once, and
/// the read-only state renders no composer and carries no loop verbs.
pub async fn run_view(
    entries: Vec<LogEntry>,
    label: String,
    theme: Option<String>,
    images: ImageMode,
) -> Result<()> {
    run_offline(entries, Some(label), theme, images, None, false, false).await
}

/// The shared offline runner behind [`run_gallery`] and [`run_view`]: seed the
/// events channel with a fixed entry list and run the real [`ui_loop`] over it with
/// no daemon behind it. The events sender is held for the session's life so the UI
/// never sees a `Disconnected` (which would exit immediately); the
/// submit/approve/pace/interrupt far-ends are likewise held so a send from inside
/// the state can't error. Real `ui_loop`, real rendering — just an offline log.
async fn run_offline(
    entries: Vec<LogEntry>,
    viewing: Option<String>,
    theme: Option<String>,
    images: ImageMode,
    stream: Option<gallery::stream::Script>,
    frame_cost: bool,
    form_prototype: bool,
) -> Result<()> {
    let theme = resolve_theme(theme)?;

    let (tx, rx) = crossbeam_channel::unbounded::<TuiEvent>();
    // The far ends are held for the run's life rather than consumed: an offline
    // log's images ride the entries themselves (docs/control-api.md) and there
    // is no daemon for the rest to reach, so nothing should answer them — but a
    // dropped receiver would make a send from inside the state an error.
    let (senders, receivers) = Senders::channels();

    for entry in entries {
        let _ = tx.send(TuiEvent::Entry(entry));
    }

    #[cfg(feature = "dev")]
    if form_prototype {
        let _ = tx.send(TuiEvent::SessionFormPrototype);
    }
    #[cfg(not(feature = "dev"))]
    let _ = form_prototype;

    // The emitter shares the events channel with the seeded log, so live fragments
    // land in the same UI as committed entries — the point being to watch the
    // handoff between them.
    let emitter = stream.map(|script| {
        let tx = tx.clone();
        tokio::spawn(script.play(tx))
    });

    let ui = tokio::task::spawn_blocking(move || {
        ui_loop(
            rx,
            senders,
            theme,
            images,
            viewing,
            frame_cost,
            Attached::default(),
            None,
        )
    });
    let result = ui.await.context("the TUI thread panicked")?;

    if let Some(emitter) = emitter {
        emitter.abort();
    }

    // Held until the UI exits, so the channels stay open for its life.
    drop(tx);
    drop(receivers);

    result.map(|_| ()).context("the TUI event loop failed")
}

/// Run the interactive TUI against a daemon control socket.
///
/// Subscribes to the daemon's unified `/events` stream (`?include=log,output,state`,
/// docs/control-api.md) over the [`Client`], bridges its frames into the UI thread,
/// and renders the transcript until the user quits or the daemon goes away. The
/// crossterm event loop blocks, so the UI runs on a dedicated blocking thread while
/// the SSE bridge runs as a tokio task (docs/README.md — the TUI is one client of the
/// socket).
/// The default theme — nicer than `THEME_NAMES[0]` ("1337", which only wins by
/// sort order). Overridable with `--theme` / `EFFERENCE_THEME`.
pub(super) const DEFAULT_THEME: &str = "Catppuccin Frappe";

/// Resolve the theme to apply. An explicit name must exist in the built-in set
/// (or be a `.tmTheme` path) — an unknown one is a hard error listing the
/// options. With no override, prefer [`DEFAULT_THEME`], falling back to whatever
/// sorts first if the theme set doesn't include it (e.g. submodules not fetched).
pub(super) fn resolve_theme(theme: Option<String>) -> Result<String> {
    let available = RenderContext::available_themes();
    match theme {
        Some(name) => {
            if name.ends_with(".tmTheme") || available.iter().any(|t| t == &name) {
                Ok(name)
            } else {
                anyhow::bail!(
                    "unknown theme {name:?}.\nAvailable themes: {}",
                    available.join(", ")
                )
            }
        }
        None if available.iter().any(|t| t == DEFAULT_THEME) => Ok(DEFAULT_THEME.to_string()),
        None => Ok(available
            .into_iter()
            .next()
            .unwrap_or_else(|| DEFAULT_THEME.to_string())),
    }
}

/// Offline Sessions container for the shared form experiment (docs/tui.md).
#[cfg(feature = "dev")]
pub async fn run_session_form_prototype() -> Result<()> {
    run_offline(
        gallery::log(),
        None,
        None,
        ImageMode::Off,
        None,
        false,
        true,
    )
    .await
}
