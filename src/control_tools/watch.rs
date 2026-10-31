//! The agents an `efference mcp` mount watches, and what it has shown of each: the state
//! `wait` answers from (docs/operating.md).
//!
//! A session joins the watched set when the mount creates it or a `wait` names it, and
//! stays for the mount's life. One task per watched session follows its `status` topic
//! over `GET /events?include=state:status` (docs/control-api.md) and reopens the stream
//! when it drops while the host still lists the session; a session the host no longer
//! lists has ended. The set is the mount's control state: shared by every clone of its
//! executor, never logged, and gone when the mount is.
//!
//! The follower ([`follow`]) and the key that tells one decision point from the next
//! ([`mark`]) are shared with the mount's status streams (`stream.rs`), which keep their
//! own record of what they sent.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use futures_lite::StreamExt;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

use crate::client::Client;
use crate::protocol::status::{Block, Condition, Footer, Park, Reviewer, StatusView};

/// The first pause before a dropped stream is reopened; each failed reopen doubles it,
/// up to [`RETRY_CEILING`].
const RETRY_FLOOR: Duration = Duration::from_millis(250);
const RETRY_CEILING: Duration = Duration::from_secs(10);

/// What identifies one decision point of a session: its condition's kind and what tells
/// two of that kind apart. A status whose mark differs from the last one shown is new.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    /// A held call, by its id (which its handle names), and who decides it: the same
    /// call routed to another reviewer reads differently to this mount.
    Gated {
        call_id: String,
        reviewer: Reviewer,
    },
    Asking {
        handle: Option<String>,
        question: String,
    },
    /// An idle, by its closing message's seq: an idle after another turn names another
    /// entry.
    Idle {
        closing: Option<u64>,
    },
    Stalled {
        message: String,
    },
    Parked(Park),
    Blocked(Block),
    /// Ended, failed, or failed to start: reported once, then the session is dropped.
    Ended,
}

/// `view`'s mark, or `None` for a condition that is never news: a working loop, or a
/// session still starting. That matches the in-session subscription rule, where
/// `running` is observed and never posted (docs/tools.md).
pub(super) fn mark(view: &StatusView) -> Option<Mark> {
    Some(match &view.condition {
        Condition::Starting { .. } | Condition::Working { .. } => return None,
        Condition::Gated(held) => Mark::Gated {
            call_id: held.call_id.clone(),
            reviewer: held.reviewer.clone(),
        },
        Condition::Asking {
            question, handle, ..
        } => Mark::Asking {
            handle: handle.clone(),
            question: question.clone(),
        },
        Condition::Idle { closing } => Mark::Idle {
            closing: closing.as_ref().map(|closing| closing.seq),
        },
        Condition::Stalled { message, .. } => Mark::Stalled {
            message: message.clone(),
        },
        Condition::Parked { park } => Mark::Parked(park.clone()),
        Condition::Blocked { block } => Mark::Blocked(block.clone()),
        Condition::Ended | Condition::Failed { .. } | Condition::FailedToStart { .. } => {
            Mark::Ended
        }
    })
}

/// The watched set.
#[derive(Default)]
pub(super) struct Watch {
    agents: Mutex<BTreeMap<String, Watched>>,
    /// Pinged at every status a follower reads, and at an end.
    moved: Notify,
}

/// One watched session.
struct Watched {
    /// The status its follower read last; `None` before the first.
    latest: Option<StatusView>,
    /// The mark of the status this mount showed last; `None` before the first.
    shown: Option<Mark>,
    follower: AbortHandle,
}

/// What a wait on the watched set came to.
pub(super) enum Next {
    /// Nothing is watched.
    Nothing,
    /// The statuses not shown before, in id order, each now marked shown.
    New(Vec<StatusView>),
    /// The wait ran out with nothing new: each watched session's id and latest status,
    /// `None` before its first.
    Unchanged(Vec<(String, Option<StatusView>)>),
}

impl Watch {
    fn agents(&self) -> MutexGuard<'_, BTreeMap<String, Watched>> {
        self.agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether `id` is watched.
    pub(super) fn contains(&self, id: &str) -> bool {
        self.agents().contains_key(id)
    }

    /// Watch `id`, reached through `host` (a client at the host's socket root). `shown`
    /// is the status the mount just showed of it, or `None` when it has shown none. A
    /// session already watched keeps what was shown of it.
    pub(super) fn add(self: &Arc<Self>, host: &Client, id: &str, shown: Option<&StatusView>) {
        let mut agents = self.agents();
        if agents.contains_key(id) {
            return;
        }
        let follower = tokio::spawn(follow_for(
            Arc::downgrade(self),
            host.clone(),
            id.to_string(),
        ));
        agents.insert(
            id.to_string(),
            Watched {
                latest: shown.cloned(),
                shown: shown.and_then(mark),
                follower: follower.abort_handle(),
            },
        );
    }

    /// The statuses not shown yet, waited for up to `timeout` when there are none. Each
    /// returned status is marked shown, and an ended session leaves the set once its end
    /// is returned.
    pub(super) async fn next(&self, timeout: Duration) -> Next {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Armed before the read, so a status landing between the read and the
            // await wakes it.
            let moved = self.moved.notified();
            tokio::pin!(moved);
            moved.as_mut().enable();
            {
                let mut agents = self.agents();
                if agents.is_empty() {
                    return Next::Nothing;
                }
                let new = take_new(&mut agents);
                if !new.is_empty() {
                    return Next::New(new);
                }
            }
            if tokio::time::timeout_at(deadline, moved).await.is_err() {
                return Next::Unchanged(
                    self.agents()
                        .iter()
                        .map(|(id, watched)| (id.clone(), watched.latest.clone()))
                        .collect(),
                );
            }
        }
    }

    /// A follower read `view` of `id`.
    fn saw(&self, id: &str, view: StatusView) {
        if let Some(watched) = self.agents().get_mut(id) {
            watched.latest = Some(view);
        }
        self.moved.notify_waiters();
    }

    /// `id` left the host. A status that already says it ended, or why, is kept.
    fn ended(&self, id: &str) {
        if let Some(watched) = self.agents().get_mut(id)
            && watched.latest.as_ref().and_then(mark) != Some(Mark::Ended)
        {
            watched.latest = Some(ended(id));
        }
        self.moved.notify_waiters();
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        for watched in self.agents().values() {
            watched.follower.abort();
        }
    }
}

/// Take every status not shown yet out of `agents`, marking each shown and dropping the
/// sessions whose end it reports.
fn take_new(agents: &mut BTreeMap<String, Watched>) -> Vec<StatusView> {
    let mut new = Vec::new();
    agents.retain(|_, watched| {
        let Some(latest) = &watched.latest else {
            return true;
        };
        let Some(now) = mark(latest) else {
            return true;
        };
        if watched.shown.as_ref() == Some(&now) {
            return true;
        }
        new.push(latest.clone());
        let ended = now == Mark::Ended;
        watched.shown = Some(now);
        if ended {
            watched.follower.abort();
        }
        !ended
    });
    new
}

/// The status of a session that left the host without a status saying it ended.
pub(super) fn ended(id: &str) -> StatusView {
    StatusView {
        id: id.to_string(),
        condition: Condition::Ended,
        footer: Footer::default(),
    }
}

/// What a follower reads of one session.
pub(super) enum Followed {
    /// A status its `status` topic carried; the first is the status at connect. Boxed
    /// because a reading may wait in a stream connection's queue.
    Status(Box<StatusView>),
    /// The host no longer lists it: it ended. Nothing follows this.
    Left,
    /// Its stream dropped or would not open, and the host could not be listed.
    Unreachable,
}

/// Follow `id`'s `status` topic through `host` (a client at the host's socket root),
/// handing `on` each reading, until the session leaves the host or `on` breaks. A stream
/// that drops while the host still lists the session is reopened, after a pause that
/// doubles with each failed reopen; a host that cannot be listed is read as
/// [`Followed::Unreachable`] and, unless `on` breaks, asked again after the same pause.
pub(super) async fn follow(
    host: Client,
    id: &str,
    mut on: impl FnMut(Followed) -> ControlFlow<()>,
) {
    let session = host.clone().with_prefix(&format!("/sessions/{id}"));
    let mut pause = RETRY_FLOOR;
    loop {
        if let Ok(mut events) = session.events(None, Some("state:status")).await {
            while let Some(Ok(event)) = events.next().await {
                let Some(view) = status_frame(event.data()) else {
                    continue;
                };
                if on(Followed::Status(Box::new(view))).is_break() {
                    return;
                }
                pause = RETRY_FLOOR;
            }
        }
        // The stream ended or would not open: the session left the host, or the
        // connection dropped.
        match host.list_sessions().await {
            Ok(ids) if !ids.iter().any(|held| held == id) => {
                let _ = on(Followed::Left);
                return;
            }
            Ok(_) => {}
            Err(_) => {
                if on(Followed::Unreachable).is_break() {
                    return;
                }
            }
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(RETRY_CEILING);
    }
}

/// Follow `id` for `watch` while the mount holds it. A host that cannot be listed is
/// asked again later: `wait` reports nothing new meanwhile.
async fn follow_for(watch: Weak<Watch>, host: Client, id: String) {
    follow(host, &id, |followed| {
        let Some(watch) = watch.upgrade() else {
            return ControlFlow::Break(());
        };
        match followed {
            Followed::Status(view) => watch.saw(&id, *view),
            Followed::Left => watch.ended(&id),
            Followed::Unreachable => {}
        }
        ControlFlow::Continue(())
    })
    .await;
}

/// The status view a `state` frame for the `status` topic carries.
fn status_frame(data: &str) -> Option<StatusView> {
    let frame: serde_json::Value = serde_json::from_str(data).ok()?;
    if frame["class"] != "state" || frame["kind"] != "status" {
        return None;
    }
    serde_json::from_value(frame.get("data")?.clone()).ok()
}

#[cfg(test)]
mod test;
