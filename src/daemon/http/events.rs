//! `/events`: the live tail of the append-only log as SSE (docs/control-api.md).

use super::*;

/// The validated inputs of one `/events` request, handed from the route's validating
/// half to its `Sse` half through conn state.
struct EventsRequest {
    shared: Arc<Session>,
    include: Include,
    since: u64,
    presence: Presence,
}

/// Live tail of the append-only log (docs/README.md — the log *is* the event
/// store). `?since=<seq>` replays from there then follows; default 0 streams
/// the whole session and then live. Each event carries `id: <seq>`, so a
/// reconnecting client resumes with `?since=<last_seq + 1>` — the explicit
/// cursor the SSE crate's bare framing leaves to us.
///
/// The route is a tuple because the `Sse` handler cannot answer with the
/// typed envelope: its `connect` must return a stream, so rejection
/// happens *before* it, and a failure halts so the SSE half never runs.
/// The `Sse` handler negotiates on `Accept` (a request not naming
/// `text/event-stream` passes through untouched), drives the stream
/// through its upgrade hook, and sends the heartbeat, which needs the
/// runtime the handler obtains at server init.
pub(super) fn events(heartbeat: Duration) -> impl trillium::Handler {
    (
        validate,
        sse(|conn: &mut Conn| match conn.take_state::<EventsRequest>() {
            Some(EventsRequest {
                shared,
                include,
                since,
                presence,
            }) => control_stream(shared, include, since, presence),
            // Unreachable: the validating half halts before this runs.
            None => Box::pin(futures_lite::stream::empty()),
        })
        .with_heartbeat(heartbeat),
    )
}

/// The validating half: parse the query into an [`EventsRequest`] on the conn, or halt
/// with the typed error envelope.
async fn validate(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let parsed = Include::parse(parse_param(conn.querystring(), "include")).and_then(|include| {
        // A non-human consumer opts out of the watched count
        // (docs/web-push.md); an unspelled value is rejected the
        // same way an unknown `include` token is.
        let presence = Presence::parse(parse_param(conn.querystring(), "presence"))?;
        Ok((include, presence))
    });
    match parsed {
        Ok((include, presence)) => {
            // `?since` past the head is a *wait*, not an error — the
            // natural reading of a cursor on an append-only log, and
            // the heartbeat is what keeps that wait distinguishable
            // from a dead connection.
            let since = parse_since(conn.querystring()).unwrap_or(0);
            conn.with_state(EventsRequest {
                shared,
                include,
                since,
                presence,
            })
        }
        Err(message) => fail(conn, ErrorCode::BadRequest, message),
    }
}
