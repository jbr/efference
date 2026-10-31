//! When the daemon sends push, and what it says (docs/web-push.md).
//!
//! Three moments mean the session is waiting on a person: an approval gate opened
//! ([`Session::register_pending`]), an `ask` was posed ([`Session::begin_ask`]), and
//! a turn ended with the loop going idle ([`Session::going_idle`]). Each calls
//! [`Session::moment`] with the session that answers it — the session's supervisor,
//! or for a gate its reviewer — and only a moment the operator answers reaches
//! [`Session::push`], which sends or schedules the send and returns. The turn loop
//! never waits on a push service, and a failed send is a log line rather than
//! anything the session notices. A held gate whose reviewer moves is a moment again
//! ([`Session::gate_rerouted`]).

use super::*;
use crate::log::Telemetry;
use crate::log::{
    AssistantOutput, DeviceDelivery, LogEntry, MomentKind, Payload, PushCancel, PushEvent,
    PushRecord,
};
use std::future::Future;
use std::sync::Weak;
use std::sync::atomic::AtomicU64;

/// The most a notification body carries. A lock screen shows about four lines;
/// past that the text is truncated by the platform anyway, and the full turn is
/// one tap away.
const BODY_MAX_CHARS: usize = 200;

/// How a push reaches the push services: the real one reads the stored
/// subscriptions; tests install a stub that answers for a pretend device list.
pub(crate) type PushTransport = Arc<
    dyn Fn(crate::push::Message) -> Pin<Box<dyn Future<Output = Vec<DeviceDelivery>> + Send>>
        + Send
        + Sync,
>;

/// The session's push bookkeeping: its own handle (the spawned timer and send
/// tasks append to the log through it), the moment counter, and the optional
/// stand-in for the push services.
pub(super) struct PushState {
    me: Weak<Session>,
    next: AtomicU64,
    transport: std::sync::OnceLock<PushTransport>,
}

impl PushState {
    pub(super) fn new(me: Weak<Session>) -> Self {
        Self {
            me,
            next: AtomicU64::new(1),
            transport: std::sync::OnceLock::new(),
        }
    }

    pub(super) fn me(&self) -> Weak<Session> {
        self.me.clone()
    }
}

impl MomentKind {
    /// What a notification is titled for this kind of moment. An `ask` and a
    /// settle share a title: either is the session saying something to a person.
    fn title(self) -> &'static str {
        match self {
            Self::Gate => "Gate",
            Self::Ask | Self::Settle => "Message",
        }
    }
}

impl Session {
    /// What a notification is titled: the event and the session's working
    /// directory — `Gate in ~/code/efference` — which together say what happened
    /// and which session it happened in. The path is home-relative because the
    /// home prefix is the same on every notification from one machine. Before
    /// boot sets an identity, the event stands alone.
    fn push_title(&self, event: &str) -> String {
        match self.identity.lock().unwrap().as_ref() {
            Some(identity) => format!("{event} in {}", tildify(&identity.cwd)),
            None => event.to_string(),
        }
    }

    /// Notify about one moment, in the background (docs/web-push.md).
    ///
    /// When no client is attending the host ([`Session::attentive_clients`]) the
    /// push is sent now. Otherwise it is held for
    /// [`PUSH_DELAY`](super::delayed_push::PUSH_DELAY) and sent only if neither
    /// resolution nor a sighting cancelled it ([`Session::cancel_delayed_push`]) — a
    /// push that fires is one nobody attended to. The decision lives here rather than
    /// at the call sites, so a gate, an `ask`, and a settle are delayed identically.
    /// Each step is appended to the log as a [`Telemetry::Push`].
    pub(super) fn push(&self, kind: MomentKind, body: String) {
        let moment = self.pushes.next.fetch_add(1, Ordering::Relaxed);
        let mut message = crate::push::Message::new(self.push_title(kind.title()), body);
        // The notification opens this session, not the bind's root: the path
        // the web client addresses this session by (docs/web-push.md).
        message.path = self.web_path().to_string();
        let attentive = self.attentive_clients();
        if attentive.is_empty() {
            // A newer moment supersedes a pending older one, sent or not.
            self.cancel_delayed_push(PushCancel::Superseded);
            self.record_push(moment, kind, PushEvent::Sent);
            self.send_in_background(moment, kind, message);
        } else {
            self.schedule_delayed_push(moment, kind, message, attentive);
        }
    }

    /// One moment, for the session `to` that answers it, or for the operator when `to`
    /// is `None` (docs/web-push.md). The operator's is pushed ([`Session::push`]); one a
    /// session answers reaches it through its subscription, so it is recorded as
    /// [`PushEvent::NotSent`] and pushes nothing. Either way it is the newest moment, so
    /// it supersedes a pending delayed push.
    pub(super) fn moment(&self, kind: MomentKind, to: Option<String>, body: String) {
        let Some(to) = to else {
            return self.push(kind, body);
        };
        let moment = self.pushes.next.fetch_add(1, Ordering::Relaxed);
        self.cancel_delayed_push(PushCancel::Superseded);
        self.record_push(moment, kind, PushEvent::NotSent { to });
    }

    /// The held gate's moment, when its reviewer has moved since its last one
    /// (docs/web-push.md). The status watcher (`subscriptions.rs`) calls this when the
    /// gate's routing changes: a gate that comes to rest with the operator — its
    /// supervisor ended or the pointer cleared, the dial turned to the operator, or the
    /// session it was escalated to ended — is pushed, and one that moves to a session
    /// is recorded as not sent. Nothing happens when the reviewer has not moved, or when
    /// the gate or its escalations changed between the read and the check: whatever
    /// changed them is a moment of its own or wakes the watcher again.
    pub(super) fn gate_rerouted(&self) {
        let read = {
            let pending = self.pending.lock().unwrap();
            pending.as_ref().map(|pending| {
                (
                    pending.gate.call_id.clone(),
                    pending.gate.escalations.clone(),
                )
            })
        };
        let Some((call_id, escalations)) = read else {
            return;
        };
        // Read outside the gate's lock: it asks the host about other sessions.
        let reviewer = self.gate_reviewer(&escalations);
        let body = {
            let mut pending = self.pending.lock().unwrap();
            let Some(pending) = pending.as_mut().filter(|pending| {
                pending.gate.call_id == call_id
                    && pending.gate.escalations == escalations
                    && pending.moment_to != reviewer
            }) else {
                return;
            };
            pending.moment_to = reviewer.clone();
            pending.push_body()
        };
        self.moment(MomentKind::Gate, reviewer, body);
    }

    /// Append one step of a moment's push to the log. A failed write is
    /// reported and dropped: the record must not fail what it describes.
    pub(super) fn record_push(&self, moment: u64, kind: MomentKind, event: PushEvent) {
        let turn = self.latest_turn();
        let record = PushRecord {
            moment,
            kind,
            event,
        };
        if let Err(e) = self.append(turn, Payload::Telemetry(Telemetry::Push(record))) {
            log::warn!("push record: append failed: {e}");
        }
    }

    /// Spawn the actual send and record the push services' answers when they
    /// arrive, or do nothing in the two contexts where there is nothing to do:
    /// no root is installed (push credentials derive from it, and only a binding
    /// host installs one — tests, offline verbs, and a socket-only host look
    /// alike here), and no tokio runtime is running. Both checks happen before
    /// the store is touched. A [`PushEvent::Sent`] or [`PushEvent::Released`]
    /// with no [`PushEvent::Delivery`] after it is the first case.
    pub(super) fn send_in_background(
        &self,
        moment: u64,
        kind: MomentKind,
        message: crate::push::Message,
    ) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let send: Pin<Box<dyn Future<Output = Vec<DeviceDelivery>> + Send>> =
            match self.pushes.transport.get() {
                Some(transport) => transport(message),
                None => {
                    let Some(root) = crate::auth::Root::installed() else {
                        return;
                    };
                    Box::pin(async move {
                        let store = crate::push::Store::machine_scoped();
                        crate::push::send(&store, &root.vapid_key(), message).await
                    })
                }
            };
        let me = self.pushes.me();
        handle.spawn(async move {
            let devices = send.await;
            if let Some(session) = me.upgrade() {
                session.record_push(moment, kind, PushEvent::Delivery { devices });
            }
        });
    }

    /// Replace the push services with a stub.
    #[cfg(test)]
    pub(crate) fn set_push_transport(&self, transport: PushTransport) {
        let _ = self.pushes.transport.set(transport);
    }
}

/// A path with the home directory abbreviated to `~`, as a prompt would print it.
fn tildify(path: &str) -> String {
    if let Some(home) = std::env::home_dir()
        && let Some(rest) = path.strip_prefix(&*home.to_string_lossy())
    {
        return format!("~{rest}");
    }
    path.to_string()
}

/// What a gate's notification says: the call's name, and the call's own `intent`
/// — why the model made the call, in its words (docs/tools.md) — which is what
/// decides on a locked phone whether the gate is worth unlocking for. The
/// spotter's reason is the fallback when the intent is absent (no reader may
/// assume it is present): it describes the risk rather than the purpose, which
/// is the less useful half at a glance but still beats the name alone.
///
/// A bound `agents__decide` names the child and the child's held move instead
/// (docs/permissions.md): `approve <child>'s <leaf> — <intent>`, so the
/// notification answers "whose gate, and for what" rather than showing an id.
pub(super) fn gate_body(
    name: &str,
    intent: Option<&str>,
    reason: Option<&str>,
    bound: Option<&crate::protocol::agents::BoundAction>,
) -> String {
    if let Some(bound) = bound {
        let display = crate::mcp::display_name(&bound.name);
        let text = bound.intent.as_deref().or(intent).or(reason);
        return match text {
            Some(text) => format!("approve {}'s {display} — {text}", bound.session),
            None => format!(
                "approve {}'s {display} is waiting for a decision",
                bound.session
            ),
        };
    }
    match intent.or(reason) {
        Some(text) => format!("{name} — {text}"),
        None => format!("{name} is waiting for a decision"),
    }
}

/// What the notification of a gate escalated to the operator says (docs/web-push.md):
/// who escalated it and their note, then the gate's own body ([`gate_body`]), cut to
/// fit. `source` is the escalator as a turn's `source` spells it; the operator's own
/// command line sends none.
pub(super) fn escalated_body(source: Option<&str>, note: Option<&str>, gate: &str) -> String {
    let by = source.map_or_else(
        || "the operator".to_string(),
        |source| crate::frontend::source_name(source).to_string(),
    );
    let note = note
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map_or_default(|note| format!(" (\"{note}\")"));
    truncate(&format!("escalated by {by}{note}: {gate}"))
}

/// What a settle's notification says: the turn's own closing prose, truncated —
/// what the model actually said is what decides whether the settle is worth
/// unlocking for, the same role the spotter reason plays on a gate.
///
/// The last entry a settled turn leaves is its final `Assistant` prose, so only
/// that entry is consulted: reaching further back would caption this turn with an
/// earlier one's text. Empty when the tail has no usable prose (an errored or
/// empty turn) — the title already says a turn settled, and repeating it says
/// nothing twice.
pub(super) fn settle_body(entries: &[LogEntry]) -> String {
    let Some((_, output)) = closing_output(entries) else {
        return String::new();
    };
    let prose = output.text().join(" ");
    truncate(prose.trim())
}

/// The entry a settled turn's closing prose is read from, with its `seq`: the last
/// `Assistant` entry, and only that one. `None` when that entry recorded a failure —
/// a failed tail is no prose, not a reach further back, since an earlier entry's
/// prose would caption this turn with another one's text. The push caption above
/// and the idle status (`status/view.rs`) both read it.
pub(super) fn closing_output(entries: &[LogEntry]) -> Option<(Seq, &AssistantOutput)> {
    let (seq, output, failure) = entries.iter().rev().find_map(|e| match &e.payload {
        Payload::Assistant {
            output, excluded, ..
        } => Some((e.seq, output, excluded)),
        _ => None,
    })?;
    failure.is_none().then_some((seq, output))
}

/// The body of the push an `ask` sends: the question, cut to fit.
pub(super) fn ask_body(question: &str) -> String {
    truncate(question.trim())
}

/// The first [`BODY_MAX_CHARS`] characters, on a char boundary, with an ellipsis
/// marking the cut.
fn truncate(s: &str) -> String {
    if s.chars().count() <= BODY_MAX_CHARS {
        return s.to_string();
    }
    let cut: String = s.chars().take(BODY_MAX_CHARS).collect();
    format!("{}…", cut.trim_end())
}
