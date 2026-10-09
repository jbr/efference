//! The client's seam for recording a request's lifecycle (docs/log-and-projection.md).
//!
//! Every upstream request passes `Client::send` (or the Messages keepalive beside
//! it), so that is where its stages are noticed: handed to the transport, an attempt
//! refused, the first stream event, the end. The client holds a [`StageSink`] the
//! session installs and a [`RequestOrigin`] the caller tags it with; the sink turns a
//! stage into a log entry, which the client cannot append itself.
//!
//! A [`Trace`] is one request's handle on the sink. It closes exactly once; a trace
//! dropped open — the request's future was cancelled — closes as `interrupted`, so a
//! cancelled request does not stay open in the `current_operations` view.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::log::{RequestOrigin, RequestStage, RequestStageKind};

/// Where a client reports request stages. The session implements it by minting ids
/// and appending a [`RequestStage`] entry; a client with none installed records
/// nothing.
pub trait StageRecorder: Send + Sync {
    /// A fresh request id, unique within the session.
    fn mint(&self) -> u64;
    /// Record one stage.
    fn record(&self, stage: RequestStage);
}

/// The shared handle a client carries.
pub type StageSink = Arc<dyn StageRecorder>;

/// The request in flight on a client, for the transport's retry handler, which sees
/// attempts but not the request that made them (the retry handler builds each
/// follow-up on a fresh connection). Requests that overlap on one client family are
/// attributed to the later one; the stage this feeds is advisory.
pub(super) type ActiveTrace = Arc<Mutex<Option<Arc<TraceCore>>>>;

pub(super) struct TraceCore {
    sink: StageSink,
    request: u64,
    origin: RequestOrigin,
    attempts: AtomicU32,
}

impl TraceCore {
    fn record(&self, stage: RequestStageKind) {
        self.sink
            .record(RequestStage::new(self.request, self.origin, stage));
    }

    /// An attempt got a non-success status.
    pub(super) fn attempt_failed(&self, status: u16) {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed) + 1;
        self.record(RequestStageKind::AttemptFailed { attempt, status });
    }
}

/// How much of a failure's text a `failed` stage keeps: the entry says that the
/// request failed and roughly why; the full text is on the assistant entry that
/// records the failed attempt.
const REASON_CAP: usize = 160;

/// One request's lifecycle on a sink; see the module docs.
pub(super) struct Trace {
    core: Arc<TraceCore>,
    active: ActiveTrace,
    closed: bool,
}

impl Trace {
    /// Start tracing a request, or `None` when the client has no sink.
    pub(super) fn begin(
        sink: Option<&StageSink>,
        origin: RequestOrigin,
        active: &ActiveTrace,
    ) -> Option<Self> {
        let sink = sink?;
        let core = Arc::new(TraceCore {
            sink: Arc::clone(sink),
            request: sink.mint(),
            origin,
            attempts: AtomicU32::new(0),
        });
        *active.lock().unwrap() = Some(Arc::clone(&core));
        Some(Self {
            core,
            active: Arc::clone(active),
            closed: false,
        })
    }

    pub(super) fn sent(&self) {
        self.core.record(RequestStageKind::Sent);
    }

    pub(super) fn first_byte(&self) {
        self.core.record(RequestStageKind::FirstByte);
    }

    /// The stream finalized; `finish` is the provider's word for why, when it gave one.
    pub(super) fn ended(mut self, finish: Option<&str>) {
        self.closed = true;
        self.core.sink.record(RequestStage {
            finish: finish.map(str::to_string),
            ..RequestStage::new(self.core.request, self.core.origin, RequestStageKind::Ended)
        });
        self.release();
    }

    pub(super) fn failed(mut self, reason: &str) {
        let reason = match reason.char_indices().nth(REASON_CAP) {
            Some((end, _)) => format!("{}…", &reason[..end]),
            None => reason.to_string(),
        };
        self.close(RequestStageKind::Failed { reason });
    }

    pub(super) fn interrupted(self) {
        self.failed("interrupted");
    }

    fn close(&mut self, stage: RequestStageKind) {
        self.closed = true;
        self.core.record(stage);
        self.release();
    }

    /// Stop being the client's active request, if this one still is.
    fn release(&mut self) {
        let mut active = self.active.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|core| core.request == self.core.request)
        {
            *active = None;
        }
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        if !self.closed {
            self.close(RequestStageKind::Failed {
                reason: "interrupted".into(),
            });
        }
    }
}
