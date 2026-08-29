//! When the daemon sends push, and what it says (docs/web-push.md).
//!
//! Two moments mean the session is waiting on a person: an approval gate opened
//! ([`Shared::register_pending`]) and a turn ended with the loop parking
//! ([`Shared::parking`]). Both call [`Shared::push`], which spawns the send and
//! returns — the turn loop never waits on a push service, and a failed send is a
//! log line rather than anything the session notices.

use super::*;
use crate::log::{ContentBlock, LogEntry, Payload};

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
    /// Returns immediately in the two cases where there is nothing to do: no root
    /// secret is injected (push credentials derive from it, and its absence is
    /// also what every non-serving context looks like — tests, offline verbs),
    /// and no tokio runtime is running. Both checks are cheap and happen before
    /// the store is touched, so a session that never enabled push pays nothing
    /// per gate.
    pub(super) fn push(&self, event: &str, body: String) {
        let Some(root) = crate::auth::Root::from_env() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let message = crate::push::Message::new(self.push_title(event), body);
        handle.spawn(async move {
            let store = crate::push::Store::machine_scoped();
            crate::push::send(&store, &root.vapid_key(), message).await;
        });
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
            text,
            upstream_failure,
            ..
        } => Some((text, upstream_failure)),
        _ => None,
    });
    // A failed tail is an empty body, not a reach further back: an earlier
    // entry's prose would caption this turn with another one's text.
    let Some((text, None)) = tail.map(|(text, failure)| (text, failure.as_ref())) else {
        return String::new();
    };
    let prose: String = text
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
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
