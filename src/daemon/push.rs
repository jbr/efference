//! When the daemon sends push, and what it says (docs/web-push.md).
//!
//! Two moments mean the session is waiting on a person: an approval gate opened
//! ([`Shared::register_pending`]) and a turn ended with the loop parking
//! ([`Shared::parking`]). Both call [`Shared::push`], which spawns the send and
//! returns — the turn loop never waits on a push service, and a failed send is a
//! log line rather than anything the session notices.

use super::*;
use crate::log::{LogEntry, Payload};

/// The most a notification body carries. A lock screen shows about four lines;
/// past that the text is truncated by the platform anyway, and the full turn is
/// one tap away.
const BODY_MAX_CHARS: usize = 200;

impl Shared {
    /// What a notification is titled: the event and the session's working
    /// directory — `Gate in ~/code/efference` — which together say what happened
    /// and which session it happened in. The path is home-relative because the
    /// home prefix is the same on every notification from one machine. Before
    /// `serve` sets an identity, the event stands alone.
    fn push_title(&self, event: &str) -> String {
        match self.identity.lock().unwrap().as_ref() {
            Some(identity) => format!("{event} in {}", tildify(&identity.cwd)),
            None => event.to_string(),
        }
    }

    /// Fire one push, in the background.
    ///
    /// When someone is watching the session ([`Shared::watched`],
    /// docs/web-push.md), the message is not sent but **stashed** — see
    /// [`Shared::replay_suppressed`] for when a stashed message goes out after
    /// all. The watched check is an atomic load and goes first, so a session
    /// that never enabled push pays almost nothing per gate.
    ///
    /// Suppression lives here rather than at either call site, so a gate and a
    /// settle suppress identically: both are notifications, enabled and disabled
    /// together, and no caller can grow its own exception.
    pub(super) fn push(&self, event: &str, body: String) {
        let message = crate::push::Message::new(self.push_title(event), body);
        if self.watched() {
            *self.suppressed.lock().unwrap() = Some(Suppressed {
                at: std::time::Instant::now(),
                message,
            });
            // The last watcher may have been reaped between the check and the
            // stash; its guard's drop saw nothing to replay, so re-check here or
            // the message is stranded.
            if !self.watched() {
                self.replay_suppressed();
            }
            return;
        }
        self.spawn_send(message);
    }

    /// Send a suppressed message after all, if the last watcher just left and the
    /// suppression was recent.
    ///
    /// Called by the [`Watching`](super::presence::Watching) guard whose drop takes
    /// the watched count to zero. The daemon learns a connection died only when a
    /// write to it fails — the next event or heartbeat, up to
    /// [`EVENTS_HEARTBEAT`](super::http) later — so an event can be suppressed on
    /// the strength of a connection whose client already closed it, and nobody ever
    /// saw the gate. Within [`REPLAY_WINDOW`] of the suppression, "watched it
    /// happen" and "fired into a dead connection" are indistinguishable from here,
    /// and a possible duplicate notification beats a gate nobody was told about;
    /// past the window, the operator demonstrably had the event on screen, and an
    /// event already seen is not re-notified (docs/web-push.md).
    pub(super) fn replay_suppressed(&self) {
        if let Some(message) = self.take_replayable() {
            self.spawn_send(message);
        }
    }

    /// Take the stashed message if it is still worth sending. Split from
    /// [`Shared::replay_suppressed`] so the window arithmetic is testable without
    /// observing a send.
    fn take_replayable(&self) -> Option<crate::push::Message> {
        let suppressed = self.suppressed.lock().unwrap().take()?;
        (suppressed.at.elapsed() <= REPLAY_WINDOW).then_some(suppressed.message)
    }

    /// Spawn the actual send, or do nothing in the two contexts where there is
    /// nothing to do: no root secret is injected (push credentials derive from it,
    /// and its absence is also what every non-serving context looks like — tests,
    /// offline verbs), and no tokio runtime is running. Both checks happen before
    /// the store is touched.
    fn spawn_send(&self, message: crate::push::Message) {
        let Some(root) = crate::auth::Root::from_env() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let store = crate::push::Store::machine_scoped();
            crate::push::send(&store, &root.vapid_key(), message).await;
        });
    }
}

/// A push that was suppressed because the session looked watched, kept so the
/// guard that reaps the last watcher can send it after all
/// ([`Shared::replay_suppressed`]). Only the latest is kept: a newer suppressed
/// event supersedes an older one the same way a newer notification would.
pub(super) struct Suppressed {
    at: std::time::Instant,
    message: crate::push::Message,
}

/// How recent a suppression must be for the reap of the last watcher to send it
/// after all. The bound is the daemon's blindness to a closed connection: the
/// client lingers a few seconds after the tab hides (`HIDDEN_LINGER_MS`,
/// web/src/session/connect.ts), and a close after that is unnoticed until the
/// next write — up to one `/events` heartbeat (15s). Suppression older than
/// their sum was watched by a connection that demonstrably outlived the
/// blindness, i.e. by a person.
const REPLAY_WINDOW: std::time::Duration = std::time::Duration::from_secs(20);

#[cfg(test)]
mod test;

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
pub(super) fn gate_body(name: &str, intent: Option<&str>, reason: Option<&str>) -> String {
    match intent.or(reason) {
        Some(text) => format!("{name} — {text}"),
        None => format!("{name} is waiting for a decision"),
    }
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
    let tail = entries.iter().rev().find_map(|e| match &e.payload {
        Payload::Assistant {
            output,
            upstream_failure,
            ..
        } => Some((output, upstream_failure)),
        _ => None,
    });
    // A failed tail is an empty body, not a reach further back: an earlier
    // entry's prose would caption this turn with another one's text.
    let Some((output, None)) = tail.map(|(output, failure)| (output, failure.as_ref())) else {
        return String::new();
    };
    let prose = output.text().join(" ");
    truncate(prose.trim())
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
