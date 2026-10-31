//! A session between its reservation and its adoption: the host's `starting` entry
//! (docs/control-api.md, docs/resumption.md).
//!
//! Creation and resume put the id in the host's map as a [`Starting`] before the env
//! hook and `boot::prepare` run, so the session is listed, addressable, and accepting
//! turns while it is prepared. It is a lifecycle state beside active and dormant, not a
//! loop phase: no loop exists yet. It serves `/state`, `/events`, and `/turns` (submit
//! and recall); every other route on the session answers `409 starting`
//! (`http.rs`).
//!
//! Preparation ends one of three ways. Started: the queued turns go to the session in
//! submission order, and every open `/events` connection continues into the session's
//! own stream. Failed: the queued turns ride the failure back for the clients to put in
//! a composer, and the connections end after the frame that carries it. Archived: the
//! preparation was cancelled on purpose, and the connections end.

use super::stream::{FrameEvent, Include, control_stream};
use super::*;
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;

use crate::protocol::channel_value;
use crate::protocol::state::{Stage, StartFailure, StartingView, VerbAnswer, Verbs};
use crate::protocol::status::{Condition, Footer, StatusView};
use crate::protocol::turns::{Landing, QueuedTurn};

/// Why preparation ended without a session, and what it hands back.
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub(crate) reason: String,
    /// Absolute Unix millis.
    pub(crate) at: u64,
    /// The turns queued while starting, in submission order.
    pub(crate) withdrawn: Vec<SubmittedTurn>,
}

#[derive(Clone)]
enum Outcome {
    Started(Arc<Session>),
    Failed(Failure),
    /// Stopped on purpose — archived, or the host shutting down — with what stopped it.
    Cancelled(&'static str),
}

#[derive(Default)]
struct Inner {
    stage: Stage,
    /// The MCP server whose authorization preparation is waiting on.
    authorizing: Option<String>,
    queue: Vec<SubmittedTurn>,
    outcome: Option<Outcome>,
}

/// One session being prepared. Shared by the host's map, the preparation task, and
/// every route and stream holding it; it outlives its map entry for as long as any of
/// them does.
pub(crate) struct Starting {
    id: String,
    project: PathBuf,
    working: PathBuf,
    resume: bool,
    since: u64,
    created_by: Option<String>,
    detached: bool,
    /// Set by an archive: the preparation is to stop. Not a child `Swansong`, which
    /// would hold the host's shutdown open for as long as anything holds this entry.
    cancelled: AtomicBool,
    cancel: Notify,
    inner: Mutex<Inner>,
    changed: Notify,
}

/// Who a starting session is: what the host knows before anything is prepared.
pub(crate) struct StartingParts {
    pub(crate) id: String,
    pub(crate) directories: crate::policy::Directories,
    /// Reopening a dormant session rather than creating one.
    pub(crate) resume: bool,
    pub(crate) created_by: Option<String>,
    pub(crate) detached: bool,
}

impl Starting {
    pub(crate) fn new(parts: StartingParts) -> Arc<Self> {
        Arc::new(Self {
            id: parts.id,
            project: parts.directories.project().to_path_buf(),
            working: parts.directories.working().to_path_buf(),
            resume: parts.resume,
            since: crate::log::Timestamp::now().0,
            created_by: parts.created_by,
            detached: parts.detached,
            cancelled: AtomicBool::new(false),
            cancel: Notify::new(),
            inner: Mutex::new(Inner::default()),
            changed: Notify::new(),
        })
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// The session's project: the root whose `.efference` holds it.
    pub(crate) fn project(&self) -> &std::path::Path {
        &self.project
    }

    pub(crate) fn working(&self) -> &std::path::Path {
        &self.working
    }

    pub(crate) fn resume(&self) -> bool {
        self.resume
    }

    pub(crate) fn created_by(&self) -> Option<&str> {
        self.created_by.as_deref()
    }

    pub(crate) fn detached(&self) -> bool {
        self.detached
    }

    /// Ask the preparation to stop (an archive while starting).
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.cancel.notify_waiters();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves once [`cancel`](Self::cancel) has been called.
    pub(crate) async fn cancelled(&self) {
        loop {
            let notified = self.cancel.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.cancelled.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    fn touch(&self) {
        self.changed.notify_waiters();
    }

    pub(crate) fn set_stage(&self, stage: Stage) {
        self.inner.lock().unwrap().stage = stage;
        self.touch();
    }

    /// Name the MCP server preparation waits on for authorization, or clear it.
    pub(crate) fn set_authorizing(&self, server: Option<&str>) {
        self.inner.lock().unwrap().authorizing = server.map(str::to_string);
        self.touch();
    }

    /// Take a turn: queued while starting, delivered to the session once it has
    /// started, refused after a failure with the reason.
    pub(crate) fn submit(&self, turn: SubmittedTurn) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        match &inner.outcome {
            None => inner.queue.push(turn),
            Some(Outcome::Started(session)) => session.submit_client_turn(turn),
            Some(Outcome::Failed(_) | Outcome::Cancelled(_)) => return Err(self.refusal_in(&inner)),
        }
        drop(inner);
        self.touch();
        Ok(())
    }

    /// Withdraw the queued suffix beginning at `submission_id` (`DELETE /turns/:id`).
    pub(crate) fn withdraw(&self, submission_id: &str) -> Vec<SubmittedTurn> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(Outcome::Started(session)) = &inner.outcome {
            return session.withdraw_turns(submission_id);
        }
        let popped = match inner
            .queue
            .iter()
            .position(|turn| turn.submission_id == submission_id)
        {
            Some(position) => inner.queue.split_off(position),
            None => Vec::new(),
        };
        drop(inner);
        if !popped.is_empty() {
            self.touch();
        }
        popped
    }

    /// Hand the queue to the started session, in submission order. Held under the lock
    /// so a turn submitted meanwhile is either in the queue handed over or delivered
    /// after it, never ahead of it. The first outcome recorded is the one that holds;
    /// each recorder returns whether it was first.
    pub(crate) fn started(&self, session: Arc<Session>) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.outcome.is_some() {
            return false;
        }
        for turn in std::mem::take(&mut inner.queue) {
            session.submit_client_turn(turn);
        }
        inner.authorizing = None;
        inner.outcome = Some(Outcome::Started(session));
        drop(inner);
        self.touch();
        true
    }

    /// Record the failure; the queued turns ride it back.
    pub(crate) fn failed(&self, reason: String) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.outcome.is_some() {
            return false;
        }
        let failure = Failure {
            reason,
            at: crate::log::Timestamp::now().0,
            withdrawn: std::mem::take(&mut inner.queue),
        };
        inner.authorizing = None;
        inner.outcome = Some(Outcome::Failed(failure));
        drop(inner);
        self.touch();
        true
    }

    /// Record a cancellation on purpose: `by` is what stopped it, an archive or the
    /// host's shutdown.
    pub(crate) fn cancelled_by(&self, by: &'static str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.outcome.is_some() {
            return false;
        }
        inner.queue.clear();
        inner.authorizing = None;
        inner.outcome = Some(Outcome::Cancelled(by));
        drop(inner);
        self.touch();
        true
    }

    /// Wait for preparation to end: the session, or why there is none.
    pub(crate) async fn finished(&self) -> Result<Arc<Session>, String> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match &self.inner.lock().unwrap().outcome {
                Some(Outcome::Started(session)) => return Ok(Arc::clone(session)),
                Some(Outcome::Failed(failure)) => return Err(failure.reason.clone()),
                Some(Outcome::Cancelled(by)) => {
                    return Err(format!(
                        "session {:?} was stopped while starting: {by}",
                        self.id
                    ));
                }
                None => {}
            }
            changed.await;
        }
    }

    /// What a route this entry does not serve answers with (`409 starting`).
    pub(crate) fn refusal(&self) -> String {
        self.refusal_in(&self.inner.lock().unwrap())
    }

    fn refusal_in(&self, inner: &Inner) -> String {
        let id = &self.id;
        match &inner.outcome {
            Some(Outcome::Failed(failure)) => {
                format!("session {id:?} did not start: {}", failure.reason)
            }
            Some(Outcome::Cancelled(by)) => {
                format!("session {id:?} was stopped while starting: {by}")
            }
            // A route resolved just before the session started reads this entry; the
            // session itself answers the next request.
            Some(Outcome::Started(_)) => format!("session {id:?} has just started; ask again"),
            None => format!(
                "session {id:?} is starting: its environment and MCP servers are being \
                 prepared, and only a turn is accepted until it has started"
            ),
        }
    }

    /// The `starting` topic (docs/control-api.md): `None` once started.
    pub(crate) fn starting_view(&self) -> Option<StartingView> {
        let inner = self.inner.lock().unwrap();
        let failure = match &inner.outcome {
            Some(Outcome::Started(_)) => return None,
            Some(Outcome::Failed(failure)) => Some(StartFailure {
                reason: failure.reason.clone(),
                at: failure.at,
                withdrawn: failure.withdrawn.iter().map(QueuedTurn::from).collect(),
            }),
            Some(Outcome::Cancelled(_)) | None => None,
        };
        Some(StartingView {
            id: self.id.clone(),
            cwd: self.project.clone(),
            since: self.since,
            resume: self.resume,
            stage: inner.stage,
            authorizing: inner.authorizing.clone(),
            failure,
        })
    }

    /// The `verbs` topic while starting: a turn is queued, the rest are refused with
    /// the route's own message, and after a failure the turn is refused too.
    fn verbs(&self) -> Verbs {
        let inner = self.inner.lock().unwrap();
        let refused = VerbAnswer::refused(self.refusal_in(&inner));
        let turn = match inner.outcome {
            None | Some(Outcome::Started(_)) => VerbAnswer::delivered(Landing::Queued),
            _ => refused.clone(),
        };
        Verbs {
            turn,
            continue_verb: refused.clone(),
            compact: refused,
        }
    }

    fn queued(&self) -> Vec<QueuedTurn> {
        self.inner
            .lock()
            .unwrap()
            .queue
            .iter()
            .map(QueuedTurn::from)
            .collect()
    }

    /// The `status` topic while starting (docs/control-api.md): the preparation, its
    /// failure, or a cancellation, which reads as an end; `null` once started, when the
    /// session's own topic takes over.
    pub(crate) fn status_view(&self) -> Option<StatusView> {
        let inner = self.inner.lock().unwrap();
        let condition = match &inner.outcome {
            None => Condition::Starting {
                stage: inner.stage,
                authorizing: inner.authorizing.clone(),
                since: self.since,
            },
            Some(Outcome::Failed(failure)) => Condition::FailedToStart {
                reason: failure.reason.clone(),
            },
            Some(Outcome::Cancelled(_)) => Condition::Ended,
            Some(Outcome::Started(_)) => return None,
        };
        Some(StatusView {
            id: self.id.clone(),
            condition,
            footer: Footer {
                queued: inner.queue.iter().map(|turn| turn.source.clone()).collect(),
                ..Footer::default()
            },
        })
    }

    /// The topics a starting entry publishes, in [`Topic::ALL`] order.
    const TOPICS: [Topic; 4] = [Topic::Verbs, Topic::Queued, Topic::Starting, Topic::Status];

    fn topic_json(&self, topic: Topic) -> Value {
        match topic {
            Topic::Verbs => channel_value(self.verbs()),
            Topic::Queued => channel_value(self.queued()),
            Topic::Status => channel_value(self.status_view()),
            _ => channel_value(self.starting_view()),
        }
    }

    /// `GET /state` while starting: `last_seq` (null: no log is open) and the four
    /// topics a starting entry publishes. The other topics describe a loop, and are
    /// absent until the session's own stream delivers them.
    pub(crate) fn state_json(&self) -> String {
        let mut out = serde_json::Map::new();
        out.insert("last_seq".into(), Value::Null);
        for topic in Self::TOPICS {
            out.insert(topic.as_str().to_string(), self.topic_json(topic));
        }
        Value::Object(out).to_string()
    }

    fn outcome(&self) -> Option<Outcome> {
        self.inner.lock().unwrap().outcome.clone()
    }
}

/// One `/events` connection on a starting session: its frames while starting, then
/// the session's own.
enum Leg {
    Starting {
        entry: Arc<Starting>,
        include: Include,
        since: u64,
        /// What this connection last sent per topic, in [`Starting::TOPICS`] order.
        sent: [Option<String>; 4],
        ready: VecDeque<FrameEvent>,
    },
    Session(Pin<Box<dyn Stream<Item = FrameEvent> + Send + Sync + 'static>>),
    Done,
}

/// The `/events` stream of a starting session (docs/control-api.md). Level-triggered
/// frames for `verbs`, `queued`, `starting`, and `status` (those of them the connection
/// subscribed to) as they move; then, once the session has started, the session's own
/// stream from the connection's `since`, whose connect snapshot carries `starting:
/// null`. A failure or an archive ends the stream after the frames describing it.
pub(crate) fn starting_stream(
    entry: Arc<Starting>,
    include: Include,
    since: u64,
) -> Pin<Box<dyn Stream<Item = FrameEvent> + Send + Sync + 'static>> {
    let leg = Leg::Starting {
        entry,
        include,
        since,
        sent: Default::default(),
        ready: VecDeque::new(),
    };
    Box::pin(futures_lite::stream::unfold(leg, |mut leg| async move {
        loop {
            match &mut leg {
                Leg::Starting {
                    entry,
                    include,
                    since,
                    sent,
                    ready,
                } => {
                    if let Some(event) = ready.pop_front() {
                        return Some((event, leg));
                    }
                    let entry = Arc::clone(entry);
                    let changed = entry.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    // The outcome is read before the topics, so the topics read after
                    // it describe at least that outcome: a failure's frame is queued
                    // before the leg ends.
                    let outcome = entry.outcome();
                    for (slot, topic) in Starting::TOPICS.into_iter().enumerate() {
                        if !include.topics.contains(&topic) {
                            continue;
                        }
                        let json = entry.topic_json(topic).to_string();
                        if sent[slot].as_deref() != Some(json.as_str()) {
                            ready.push_back(Frame::state_raw(topic, &json).into());
                            sent[slot] = Some(json);
                        }
                    }
                    if !ready.is_empty() {
                        continue;
                    }
                    match outcome {
                        Some(Outcome::Started(session)) => {
                            leg = Leg::Session(control_stream(session, include.clone(), *since));
                        }
                        Some(Outcome::Failed(_) | Outcome::Cancelled(_)) => leg = Leg::Done,
                        None => changed.await,
                    }
                }
                Leg::Session(stream) => {
                    use futures_lite::StreamExt;
                    return stream.next().await.map(|event| (event, leg));
                }
                Leg::Done => return None,
            }
        }
    }))
}

#[cfg(test)]
mod test;
