//! `/state`: the composite current representation (docs/control-api.md).

use super::*;

/// The composite current representation — every state topic under its own
/// name (docs/control-api.md). `?wait=settled` blocks until the daemon
/// reaches a decision point — the loop phase is idle with nothing queued, gated,
/// asking, cold-parked, or blocked — then answers with the same snapshot, whose
/// `loop_phase` names which: the wait-without-acting read that replaces polling in
/// a wall-clock loop. Returns at once if already settled.
pub(super) async fn read_state(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    match parse_param(conn.querystring(), "wait") {
        Some("settled") => settle_and_state(&shared, conn).await,
        // An unknown predicate is an error, not an immediate answer: a
        // client that asked to wait must not be told "now" is what it
        // waited for.
        Some(other) => {
            let message = format!("unknown wait predicate {other:?}; try wait=settled");
            fail(conn, ErrorCode::BadRequest, message)
        }
        None if conn.querystring().split('&').any(|kv| kv == "wait") => fail(
            conn,
            ErrorCode::BadRequest,
            "wait needs a predicate: wait=settled",
        ),
        None => conn.with_status(200).with_body(shared.state_json()).halt(),
    }
}
