//! A mount's moves, as the status names them (docs/tools.md): the table the renderer
//! (`render.rs`) takes, so the calls a status text names are the reader's mount's own.

/// What a move is aimed at.
pub(crate) struct Target<'a> {
    pub(crate) session: &'a str,
    /// The held call's provider id.
    pub(crate) call_id: Option<&'a str>,
    /// The held or asking call's transcript handle.
    pub(crate) handle: Option<&'a str>,
    /// The entry a transcript read starts from.
    pub(crate) since: Option<u64>,
}

/// A mount's moves: each writes the exact call that makes it, its target filled in.
/// Swapping the table changes every verb the text names and nothing else.
pub(crate) struct Moves {
    /// Resolve a held call, with its outcomes.
    pub(crate) decide: fn(&Target) -> String,
    /// Hand a held call to the operator; `None` on a mount whose `decide` escalates.
    pub(crate) hand_back: Option<fn(&Target) -> String>,
    /// Send the session its next message.
    pub(crate) message: fn(&Target) -> String,
    /// Answer the question the session is asking.
    pub(crate) answer: fn(&Target) -> String,
    /// Read the session's transcript from an entry.
    pub(crate) transcript: fn(&Target) -> String,
    /// Read the session's whole state.
    pub(crate) state: fn(&Target) -> String,
}

impl Moves {
    /// The in-session `sessions__*` tools (docs/tools.md).
    pub(crate) const IN_SESSION: Moves = Moves {
        decide: in_session::approve,
        hand_back: Some(in_session::hand_back),
        message: in_session::message,
        answer: in_session::answer,
        transcript: in_session::transcript,
        state: in_session::state,
    };
}

mod in_session {
    use super::Target;

    fn quoted(s: &str) -> String {
        serde_json::to_string(s).unwrap_or_default()
    }

    fn session(target: &Target) -> String {
        format!("session: {}", quoted(target.session))
    }

    pub(super) fn approve(target: &Target) -> String {
        let call_id = target
            .call_id
            .map(|id| format!(", call_id: {}", quoted(id)))
            .unwrap_or_default();
        format!(
            "sessions__approve({}{call_id}) runs it; with abort: true it is refused and the \
             turn ends; with redirect: \"<message>\" it is refused and the session goes on \
             under your message",
            session(target)
        )
    }

    pub(super) fn hand_back(target: &Target) -> String {
        format!(
            "sessions__review_level({}, level: \"spotter_human\") hands this session's gates \
             to the operator; this call stays held and goes to them",
            session(target)
        )
    }

    pub(super) fn message(target: &Target) -> String {
        format!("sessions__turn({}, text: [\"<message>\"])", session(target))
    }

    pub(super) fn answer(target: &Target) -> String {
        format!("sessions__turn({}, text: [\"<answer>\"])", session(target))
    }

    pub(super) fn transcript(target: &Target) -> String {
        let since = target
            .since
            .map(|seq| format!(", since: {seq}"))
            .unwrap_or_default();
        format!("sessions__transcript({}{since})", session(target))
    }

    pub(super) fn state(target: &Target) -> String {
        format!("sessions__state({})", session(target))
    }
}
