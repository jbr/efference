//! The channels between the UI thread and the tokio side (docs/tui.md).
//!
//! The crossterm loop blocks, so nothing in the UI awaits: every operator
//! intent is *sent*, and a dispatcher task on the tokio side turns it into the
//! control-channel call. Both halves are built at once and travel together —
//! [`Senders`] to the state, [`Receivers`] to the dispatchers — so adding an
//! intent is one field on each rather than a parameter threaded through every
//! entry point.
//!
//! An offline run (the gallery, a read-only view) holds the [`Receivers`] for
//! the session's life without reading them, which is what keeps a send from the
//! state from erroring where there is no daemon to answer.

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::{HostCmd, PaceCmd, SidebarCmd};
use crate::frontend::{Approval, Interrupt, SubmittedTurn};

/// What the UI sends the operator's intent over.
#[derive(Debug)]
pub(crate) struct Senders {
    pub submit: UnboundedSender<SubmittedTurn>,
    /// Withdraw the pending-queue suffix from a submission id
    /// (docs/control-api.md); the answer rides home as a
    /// [`TuiEvent::Recalled`](super::TuiEvent::Recalled).
    pub recall: UnboundedSender<String>,
    /// Upload or remove a staged attachment (docs/inbox.md); an upload's answer
    /// rides home as a [`TuiEvent::Uploaded`](super::TuiEvent::Uploaded).
    pub attachments: UnboundedSender<super::staged::AttachCmd>,
    /// The zero-token continue (docs/control-api.md). Its one field is the verb's
    /// one field: consent to a cold read (`accept_cold_read`).
    pub continue_loop: UnboundedSender<bool>,
    pub approve: UnboundedSender<(String, Approval)>,
    pub pace: UnboundedSender<PaceCmd>,
    pub interrupt: UnboundedSender<Interrupt>,
    pub sidebar: UnboundedSender<SidebarCmd>,
    pub images: UnboundedSender<crate::tui::image::ImageKey>,
    /// The verbs addressed to the *host* rather than to this session — list and
    /// end (docs/tui.md).
    pub host: UnboundedSender<HostCmd>,
}

/// The far ends, one per field of [`Senders`].
#[derive(Debug)]
pub(crate) struct Receivers {
    pub submit: UnboundedReceiver<SubmittedTurn>,
    pub recall: UnboundedReceiver<String>,
    pub attachments: UnboundedReceiver<super::staged::AttachCmd>,
    pub continue_loop: UnboundedReceiver<bool>,
    pub approve: UnboundedReceiver<(String, Approval)>,
    pub pace: UnboundedReceiver<PaceCmd>,
    pub interrupt: UnboundedReceiver<Interrupt>,
    pub sidebar: UnboundedReceiver<SidebarCmd>,
    pub images: UnboundedReceiver<crate::tui::image::ImageKey>,
    pub host: UnboundedReceiver<HostCmd>,
}

impl Senders {
    /// Both halves of every channel.
    pub(crate) fn channels() -> (Self, Receivers) {
        let (submit, submit_rx) = unbounded_channel();
        let (recall, recall_rx) = unbounded_channel();
        let (attachments, attachments_rx) = unbounded_channel();
        let (continue_loop, continue_rx) = unbounded_channel();
        let (approve, approve_rx) = unbounded_channel();
        let (pace, pace_rx) = unbounded_channel();
        let (interrupt, interrupt_rx) = unbounded_channel();
        let (sidebar, sidebar_rx) = unbounded_channel();
        let (images, images_rx) = unbounded_channel();
        let (host, host_rx) = unbounded_channel();
        (
            Self {
                submit,
                recall,
                attachments,
                continue_loop,
                approve,
                pace,
                interrupt,
                sidebar,
                images,
                host,
            },
            Receivers {
                submit: submit_rx,
                recall: recall_rx,
                attachments: attachments_rx,
                continue_loop: continue_rx,
                approve: approve_rx,
                pace: pace_rx,
                interrupt: interrupt_rx,
                sidebar: sidebar_rx,
                images: images_rx,
                host: host_rx,
            },
        )
    }
}
