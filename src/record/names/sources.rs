//! The labels a turn's `source` is written in (docs/log-and-projection.md,
//! docs/control-api.md): another session, an MCP client, a hand-off brief, the session's own
//! kickoff or permission review, and the readers that take a label apart.

/// The `source` of the kickoff turn a `first-turn` config submits (docs/context.md).
/// Reserved: the clients read it to leave the turn out of the transcript, since it
/// belongs to the session's opening the way the preamble does, not to the operator.
pub const KICKOFF_SOURCE: &str = "efference";

/// The `source` of a hand-off brief: the predecessor's id in the reserved
/// `handoff[…]` form (docs/tools.md). The clients render it as the brief it is
/// rather than as the operator's turn, which is why a client may not claim it.
pub fn handoff_source(predecessor: &str) -> String {
    format!("handoff[{predecessor}]")
}

/// The predecessor a hand-off brief's `source` names, or `None` for any other.
pub fn handoff_predecessor(source: &str) -> Option<&str> {
    source.strip_prefix("handoff[")?.strip_suffix(']')
}

/// The `source` of a turn or decision another session sends: `session[<id>]`, or
/// `session[<id> in <place>]` when the sender works in another project than the
/// recipient, `<place>` its project directory (docs/control-api.md).
pub fn session_source(id: &str, place: Option<&str>) -> String {
    match place {
        Some(place) => format!("session[{id} in {place}]"),
        None => format!("session[{id}]"),
    }
}

/// Whether `source` is the loop's own permission review, `review[<level>]`
/// (docs/permissions.md). The review writes the redirects no person or agent wrote, under
/// the level in force.
pub fn is_review_source(source: &str) -> bool {
    source.starts_with("review[") && source.ends_with(']')
}

/// The session a `session[<id>]` source names, or `None` for any other.
pub fn source_session(source: &str) -> Option<&str> {
    session_parts(source).map(|(id, _)| id)
}

/// A `session[…]` source's id, and the project it names when the sender worked in
/// another project than the recipient ([`session_source`]). An id holds no space, so
/// the first ` in ` ends it.
fn session_parts(source: &str) -> Option<(&str, Option<&str>)> {
    let inner = source.strip_prefix("session[")?.strip_suffix(']')?;
    Some(match inner.split_once(" in ") {
        Some((id, place)) => (id, Some(place)),
        None => (inner, None),
    })
}

/// The name inside a `session[…]`, `agent[…]` or `mcp[…]` label — a session's id
/// without the project it may name, or a client's name — and any other label as
/// written (docs/control-api.md).
pub fn source_name(source: &str) -> &str {
    if let Some(id) = source_session(source) {
        return id;
    }
    ["agent[", "mcp["]
        .iter()
        .find_map(|open| source.strip_prefix(open)?.strip_suffix(']'))
        .unwrap_or(source)
}

/// Whether a turn's `source` is another agent's — a session's (`session[<id>]`) or an MCP
/// client's (`mcp[<name>]`) — rather than one of the operator's own clients (none, or
/// `web`). An agent's turn waits behind the operator's pace hold (docs/control-channel.md).
pub fn agent_source(source: Option<&str>) -> bool {
    source.is_some_and(|source| source_session(source).is_some() || source.starts_with("mcp["))
}

/// How a turn's `source` opens it on the wire (docs/log-and-projection.md): a turn
/// from another session as `<id> (agent)`, or `<id> (agent in <place>)` from another
/// project; any other label as written.
pub fn wire_label(source: &str) -> std::borrow::Cow<'_, str> {
    match session_parts(source) {
        Some((id, None)) => format!("{id} (agent)").into(),
        Some((id, Some(place))) => format!("{id} (agent in {place})").into(),
        None => source.into(),
    }
}
