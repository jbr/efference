//! A mount's moves, as the status names them (docs/tools.md): the table the renderer
//! (`render.rs`) takes, so the calls a status text names are the reader's mount's own.
//! The two tables differ only in the tool names: `agents__<verb>` in a session's own
//! toolbox, the bare verb over `efference mcp` and the command line.

/// What a move is aimed at.
pub(crate) struct Target<'a> {
    pub(crate) session: &'a str,
    /// The held or asking call's transcript handle.
    pub(crate) handle: Option<&'a str>,
    /// The entry a transcript read zooms into: an idle session's closing message.
    pub(crate) seq: Option<u64>,
}

/// A mount's moves: each writes the exact call that makes it, its target filled in.
/// Swapping the table changes every verb the text names and nothing else.
pub(crate) struct Moves {
    /// Resolve a held call, with its outcomes.
    pub(crate) decide: fn(&Target) -> String,
    /// Pass a held call up undecided to whoever decides next (docs/permissions.md).
    pub(crate) escalate: fn(&Target) -> String,
    /// Send the session its next message.
    pub(crate) message: fn(&Target) -> String,
    /// Answer the question the session is asking.
    pub(crate) answer: fn(&Target) -> String,
    /// The empty message: a continue.
    pub(crate) resume: fn(&Target) -> String,
    /// End the session.
    pub(crate) archive: fn(&Target) -> String,
    /// Read one entry of the session's transcript in full.
    pub(crate) transcript: fn(&Target) -> String,
    /// Read the session's whole status.
    pub(crate) status: fn(&Target) -> String,
    /// The verb that starts a session, by the name this mount gives it.
    pub(crate) create: fn() -> String,
}

impl Moves {
    /// The in-session `agents__*` tools (docs/tools.md).
    pub(crate) const IN_SESSION: Moves = Moves::spelled_as::<InSession>();

    /// The `efference mcp` tools, which the command line's verbs share (docs/operating.md).
    pub(crate) const MCP: Moves = Moves::spelled_as::<Mcp>();

    /// The table whose calls are spelled under `P`'s prefix.
    const fn spelled_as<P: Prefix>() -> Moves {
        Moves {
            decide: |target| calls::decide(P::PREFIX, target),
            escalate: |target| calls::escalate(P::PREFIX, target),
            message: |target| calls::message(P::PREFIX, target),
            answer: |target| calls::answer(P::PREFIX, target),
            resume: |target| calls::resume(P::PREFIX, target),
            archive: |target| calls::archive(P::PREFIX, target),
            transcript: |target| calls::transcript(P::PREFIX, target),
            status: |target| calls::status(P::PREFIX, target),
            create: || format!("{}create", P::PREFIX),
        }
    }
}

/// A mount's tool-name prefix, as a type so a table's functions can name it without
/// capturing it.
trait Prefix {
    const PREFIX: &'static str;
}

/// A session's own toolbox: `agents__<verb>`.
struct InSession;

impl Prefix for InSession {
    const PREFIX: &'static str = "agents__";
}

/// `efference mcp` and the command line: the bare verb.
struct Mcp;

impl Prefix for Mcp {
    const PREFIX: &'static str = "";
}

/// One table's calls, spelled under `prefix`.
mod calls {
    use super::Target;

    fn quoted(s: &str) -> String {
        serde_json::to_string(s).unwrap_or_default()
    }

    fn agent(target: &Target) -> String {
        format!("agent: {}", quoted(target.session))
    }

    pub(super) fn decide(prefix: &str, target: &Target) -> String {
        let seq = target
            .handle
            .map_or_default(|handle| format!(", seq: {}", quoted(handle)));
        format!(
            "{prefix}decide({}{seq}, decision: \"allow\" | \"deny\", message?) — allow runs it, \
             and a message rides with its result; deny with a message refuses it and the session \
             goes on under your message; a bare deny ends its turn",
            agent(target)
        )
    }

    pub(super) fn escalate(prefix: &str, target: &Target) -> String {
        let seq = target
            .handle
            .map_or_default(|handle| format!(", seq: {}", quoted(handle)));
        format!(
            "{prefix}decide({}{seq}, decision: \"escalate\", message?) — leaves it held and \
             passes it to whoever decides above you, with your message as a note to them",
            agent(target)
        )
    }

    pub(super) fn message(prefix: &str, target: &Target) -> String {
        format!("{prefix}message({}, text: \"<message>\")", agent(target))
    }

    pub(super) fn answer(prefix: &str, target: &Target) -> String {
        let reply_to = quoted(target.handle.unwrap_or("<seq>"));
        format!(
            "{prefix}message({}, text: \"<answer>\", reply_to: {reply_to})",
            agent(target)
        )
    }

    pub(super) fn resume(prefix: &str, target: &Target) -> String {
        format!("{prefix}message({}, text: \"\")", agent(target))
    }

    pub(super) fn archive(prefix: &str, target: &Target) -> String {
        format!("{prefix}archive({})", agent(target))
    }

    pub(super) fn transcript(prefix: &str, target: &Target) -> String {
        let seq = target
            .seq
            .map_or_default(|seq| format!(", seq: {}", quoted(&seq.to_string())));
        format!("{prefix}transcript({}{seq})", agent(target))
    }

    pub(super) fn status(prefix: &str, target: &Target) -> String {
        format!("{prefix}status({})", agent(target))
    }
}
