//! The unified control stream — `GET /events` (docs/control-api.md).
//!
//! One connection carrying the three **durability classes** (`frames.rs` defines them)
//! in one total order.
//!
//! The model, named so it doesn't have to be re-derived: **`state` is a map of per-topic
//! LWW-registers, delivered causally with respect to the totally-ordered log.** The log is
//! a single-writer replication log, `GET /state` is a materialized view over it, and what
//! this module adds is a *consistent-cut read* against that view. Two properties follow,
//! and only one is free:
//!
//! - **Convergence** — a client's state eventually equals the daemon's. Free from
//!   whole-topic replacement: it is idempotent and order-insensitive, so any final
//!   emission repairs any history. That is also what makes per-topic coalescing legal
//!   below.
//! - **Coherence** — the state a client holds is true *as of the last `log` frame it
//!   received*. Not free. It is what [`Tail::releasable`] enforces, and it is what makes
//!   the spot→gate and parking→final-entry pairs render correctly rather than merely
//!   settle correctly.
//!
//! Coherence is the whole reason for one connection. Across three, a client could see the
//! gate before the `spot` entry whose text the gate is quoting, or `running: false` before
//! the entries that finished the turn — the second answering "is it done?" wrong, in the
//! direction a human acts on.

use super::*;
use std::collections::BTreeMap;

/// Which classes and topics a connection subscribed to (`?include=`).
///
/// The grammar is a superset rather than a second one: a token is either a bare class
/// (`log`, `output`, `state` — meaning all of it) or `class:topic` (`state:gate`). Only
/// `state` has topics today; the grammar needn't know that.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Include {
    pub(crate) log: bool,
    pub(crate) output: bool,
    /// The subscribed state topics, in [`Topic::ALL`] order.
    pub(crate) topics: Vec<Topic>,
}

impl Default for Include {
    /// Everything — what a client that omits `?include=` gets.
    fn default() -> Self {
        Self {
            log: true,
            output: true,
            topics: Topic::ALL.to_vec(),
        }
    }
}

impl Include {
    /// Parse an `?include=` value, or say which token was not understood.
    ///
    /// **An unknown token is an error, never ignored.** Silently dropping `state:gaet`
    /// yields a stream that never delivers a gate — the worst failure mode available
    /// here, because it looks like a daemon that never gated. The cost is that an old
    /// daemon rejects a newer client, which is the right direction: the client learns
    /// instead of missing state it asked for.
    pub(crate) fn parse(raw: Option<&str>) -> Result<Self, String> {
        let Some(raw) = raw else {
            return Ok(Self::default());
        };
        if raw.is_empty() {
            return Err("include must name at least one class".into());
        }
        let mut include = Self {
            log: false,
            output: false,
            topics: Vec::new(),
        };
        for token in raw.split(',') {
            match token {
                "log" => include.log = true,
                "output" => include.output = true,
                "state" => include.topics = Topic::ALL.to_vec(),
                _ => match token.split_once(':') {
                    Some(("state", name)) => {
                        let topic = Topic::parse(name)
                            .ok_or_else(|| format!("unknown state topic: {name:?}"))?;
                        if !include.topics.contains(&topic) {
                            include.topics.push(topic);
                        }
                    }
                    _ => return Err(format!("unknown include token: {token:?}")),
                },
            }
        }
        // Keep topic order canonical regardless of the order they were asked for, so a
        // connection's frames don't depend on how its query was spelled.
        include
            .topics
            .sort_by_key(|t| Topic::ALL.iter().position(|a| a == t));
        Ok(include)
    }

    fn wants_state(&self) -> bool {
        !self.topics.is_empty()
    }
}

/// One connection's view of the three sources.
struct Tail {
    shared: Arc<Shared>,
    include: Include,
    /// The next log `seq` to send. Everything below it has been delivered, which is
    /// exactly what a state watermark is compared against.
    next_log: u64,
    state_rx: broadcast::Receiver<StateUpdate>,
    output_rx: broadcast::Receiver<StreamDelta>,
    /// State updates waiting for their watermark to be passed, **coalesced per topic**: a
    /// second change to a topic replaces the queued first rather than queuing behind it.
    /// Legal because a frame is a whole-topic replacement, and it is what bounds this at
    /// the topic count rather than at the mutation rate — the thing that stops a
    /// backlogged connection growing without limit. Keyed by position in [`Topic::ALL`]
    /// so emission order is canonical rather than arrival-dependent.
    pending: BTreeMap<usize, StateUpdate>,
    /// A delta the wait in [`next_frame`](Tail::next_frame) received while parked. A
    /// completed `recv` has already consumed the message, so it must be held here and
    /// drained through the normal priority order — discarding it would drop one delta
    /// every time the connection goes idle, not only under backpressure. Always `None`
    /// when the wait is reached, because the output drain step empties it first.
    received_output: Option<StreamDelta>,
    /// Set when the last `log` frame this connection emitted was an `Assistant` entry:
    /// the response those block deltas were describing has committed, so the deltas
    /// still queued behind it are superseded. Cleared by the next log frame of any
    /// other kind. An assistant response never directly follows another — a
    /// `ToolResult` or a `User` turn always intervenes — so the drain cannot swallow
    /// the start of the next response.
    draining_blocks: bool,
}

/// Whether a delta describes a content block of the response now in flight — the
/// deltas a committed `Assistant` entry supersedes (docs/control-api.md).
///
/// `ToolOutput` and `ProcessOutput` are excluded on purpose: they are keyed by a call
/// id and a process id rather than by a block index, and a backgrounded process
/// emitting while a turn commits must still reach the client.
fn is_block_delta(delta: &StreamDelta) -> bool {
    matches!(
        delta,
        StreamDelta::AssistantText { .. }
            | StreamDelta::ThinkingText { .. }
            | StreamDelta::ToolCallStart { .. }
            | StreamDelta::ToolCallInput { .. }
    )
}

impl Tail {
    /// Queue every subscribed topic's current value — the connect snapshot, and the
    /// repair after a lagged state receiver.
    ///
    /// No special case is needed for connect: a client opens at the tail (it read
    /// `GET /log?tail=N` first), so the snapshot's watermark is already below `next_log`
    /// and it releases at once. A client that instead opens `?since=0` on a long session
    /// holds its state until the replay drains — the honest consequence of asking for a
    /// replay, and the reason coherence needs no exception.
    fn snapshot(&mut self) {
        for update in self.shared.subscribed_snapshot(&self.include.topics) {
            self.queue(update);
        }
    }

    /// Absorb everything the differ has published without blocking.
    fn absorb_state(&mut self) {
        loop {
            match self.state_rx.try_recv() {
                Ok(update) => self.queue(update),
                // Fell behind the ring. Level-triggered state makes this recoverable
                // without replay: re-read every subscribed topic and continue. The
                // updates we missed are by definition superseded by what we just read.
                Err(broadcast::error::TryRecvError::Lagged(_)) => self.snapshot(),
                Err(_) => return,
            }
        }
    }

    fn queue(&mut self, update: StateUpdate) {
        let Some(index) = Topic::ALL.iter().position(|t| *t == update.topic) else {
            return;
        };
        if !self.include.topics.contains(&update.topic) {
            return;
        }
        self.pending.insert(index, update);
    }

    /// The next state update whose watermark the log has passed, if any.
    ///
    /// `watermark < next_log` **is** the coherence rule, which is what makes it directly
    /// testable. Two cases release immediately: an empty log (`None` — nothing to wait
    /// behind), and a connection not subscribed to `log` at all, where there is no log
    /// frame for state to be coherent *with* — the watch-app case, which would otherwise
    /// wait forever on a cursor that never advances.
    fn releasable(&mut self) -> Option<StateUpdate> {
        let index = *self
            .pending
            .iter()
            .find(|(_, update)| {
                !self.include.log || update.watermark.is_none_or(|w| w < self.next_log)
            })?
            .0;
        self.pending.remove(&index)
    }

    /// The next frame, awaiting one if nothing is ready.
    ///
    /// The drain order is the contract, not an implementation detail: **durable first**.
    /// One socket means the ephemeral class *could* stall the durable one, which three
    /// sockets could not — so log frames go unconditionally, state frames only once their
    /// watermark is passed, and output only when nothing durable is pending. Priority and
    /// the watermark push in opposite directions and compose rather than stack: "log
    /// first" decides what to send when both are eligible; the watermark decides *when* a
    /// state frame becomes eligible. It cannot deadlock — a watermark is a committed
    /// `seq`, and draining the log always overtakes it. Nor can it starve: appends are
    /// single-digit Hz at worst, and output is correctly last, since a catching-up client
    /// dropping deltas for a turn that already committed is right — the committed entry
    /// is the authoritative copy.
    ///
    /// Priority makes a committed entry *overtake* its own deltas, so the drain also
    /// **drops** what it overtook: once the `Assistant` frame has gone out, the block
    /// deltas still queued for that response are superseded and are skipped rather than
    /// delivered ([`is_block_delta`], [`Tail::draining_blocks`]).
    async fn next_frame(&mut self) -> Option<Frame> {
        loop {
            // Arm before reading, so an append landing between the read and the await is
            // not lost (the log tail's discipline, and `notify_waiters` reaches any
            // `Notified` created before the call). Held through a cloned `Arc` rather
            // than through `self`, since the rest of the loop needs `&mut self`.
            let shared = Arc::clone(&self.shared);
            let appended = shared.appended.notified();

            self.absorb_state();

            if self.include.log
                && let Some(entry) = self.shared.entry_at(self.next_log)
            {
                self.next_log += 1;
                self.draining_blocks = matches!(entry.payload, Payload::Assistant { .. });
                return Some(Frame::log(&entry));
            }

            if let Some(update) = self.releasable() {
                return Some(Frame::state_raw(update.topic, &update.json));
            }

            if self.include.output {
                // Loops so a superseded delta is *skipped*, not merely un-sent: dropping
                // one must move on to the next candidate rather than fall through to the
                // wait, or the whole tail of a committed response would be delivered one
                // park at a time.
                let mut closed = false;
                loop {
                    let delta = match self.received_output.take() {
                        Some(delta) => delta,
                        None => match self.output_rx.try_recv() {
                            Ok(delta) => delta,
                            // Drop-oldest: the ring already discarded them, and the
                            // committed entries on the `log` class are the authoritative
                            // copy.
                            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                            Err(broadcast::error::TryRecvError::Closed) => {
                                closed = true;
                                break;
                            }
                            Err(broadcast::error::TryRecvError::Empty) => break,
                        },
                    };
                    // The committed entry preempts the deltas describing it
                    // (docs/control-api.md): once this connection has delivered the
                    // `Assistant` frame, a queued block delta describes a response the
                    // client has already committed, and delivering it would reopen a
                    // block the entry closed.
                    if self.draining_blocks && is_block_delta(&delta) {
                        continue;
                    }
                    return Some(Frame::output(&delta));
                }
                if closed {
                    return None;
                }
            }

            // Nothing ready: wait for whichever source moves first. A completed `recv`
            // has **consumed** its message (cancel-safety covers only the losing
            // branches), so both channel arms must capture what they received — an `_ =`
            // here would silently drop whatever arrives while the connection is parked,
            // which for state means a topic stuck stale until its next unrelated change.
            tokio::select! {
                _ = appended => {}
                update = self.state_rx.recv(), if self.include.wants_state() => {
                    match update {
                        Ok(update) => self.queue(update),
                        // Same repair as `absorb_state`: level-triggered state recovers
                        // from a lag by re-reading, never by replay.
                        Err(broadcast::error::RecvError::Lagged(_)) => self.snapshot(),
                        Err(broadcast::error::RecvError::Closed) => {}
                    }
                }
                delta = self.output_rx.recv(), if self.include.output => {
                    if let Ok(delta) = delta {
                        self.received_output = Some(delta);
                    }
                }
            }
        }
    }
}

/// A [`Frame`] as a Server-Sent Event.
///
/// `event:` is the frame's kind and `id:` is its `seq` — set on `log` frames and nowhere
/// else, so `EventSource`'s Last-Event-ID resume tracks the durable subset and ignores the
/// ephemeral ones. An `id` on an ephemeral frame would corrupt that cursor.
pub(crate) struct FrameEvent {
    kind: &'static str,
    id: Option<String>,
    data: String,
}

impl Eventable for FrameEvent {
    // Always `Some`: every frame carries its envelope. Comment-only messages — the
    // heartbeat — come from the `Sse` handler's own interleaving, never from here.
    fn data(&self) -> Option<&str> {
        Some(&self.data)
    }
    fn event_type(&self) -> Option<&str> {
        Some(self.kind)
    }
    fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

impl From<Frame> for FrameEvent {
    fn from(frame: Frame) -> Self {
        Self {
            kind: frame.kind,
            id: frame.seq.map(|seq| seq.to_string()),
            data: frame.to_json(),
        }
    }
}

/// The per-connection frame stream (docs/control-api.md).
///
/// Boxed because the `Sse` handler's `EventStream` demands `Unpin`: the raw `unfold`
/// over an async block is not.
pub(crate) fn control_stream(
    shared: Arc<Shared>,
    include: Include,
    since: u64,
) -> Pin<Box<dyn Stream<Item = FrameEvent> + Send + Sync + 'static>> {
    let mut tail = Tail {
        state_rx: shared.subscribe_state(),
        output_rx: shared.subscribe_stream(),
        shared,
        include,
        next_log: since,
        pending: BTreeMap::new(),
        received_output: None,
        draining_blocks: false,
    };
    // Subscribe *then* snapshot: an update landing in between is absorbed as a duplicate
    // replacement, which is harmless. Snapshotting first and subscribing after would drop
    // anything in the gap entirely.
    tail.snapshot();
    Box::pin(futures_lite::stream::unfold(tail, |mut tail| async move {
        tail.next_frame().await.map(|frame| (frame.into(), tail))
    }))
}

impl Shared {
    /// The current value of each named topic, stamped with one shared watermark.
    ///
    /// Values first, then `last_seq` — the same ordering [`topic_snapshot`] uses and for
    /// the same reason: a stamp that runs ahead of its values only delays release, while
    /// one that lags them would let a client render state accounting for an entry it has
    /// not been sent.
    ///
    /// [`topic_snapshot`]: Shared::topic_snapshot
    fn subscribed_snapshot(&self, topics: &[Topic]) -> Vec<StateUpdate> {
        let values: Vec<(Topic, Arc<str>)> = topics
            .iter()
            .map(|&topic| (topic, Arc::from(self.topic_json(topic).to_string())))
            .collect();
        let watermark = self.last_committed_seq();
        values
            .into_iter()
            .map(|(topic, json)| StateUpdate {
                topic,
                json,
                watermark,
            })
            .collect()
    }
}

#[cfg(test)]
mod test;
