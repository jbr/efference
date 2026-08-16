use super::*;

/// One code, one status — so two routes returning the same code can't disagree.
#[test]
fn codes_map_to_canonical_statuses() {
    assert_eq!(ErrorCode::BadRequest.status(), 400);
    assert_eq!(ErrorCode::InvalidDecision.status(), 400);
    assert_eq!(ErrorCode::Unauthorized.status(), 401);
    assert_eq!(ErrorCode::Forbidden.status(), 403);
    assert_eq!(ErrorCode::NotFound.status(), 404);
    assert_eq!(ErrorCode::GateMoved.status(), 409);
    assert_eq!(ErrorCode::RateLimited.status(), 429);
    assert_eq!(ErrorCode::Internal.status(), 500);
}

/// The envelope is `{code, message}` with `detail` omitted — not null — when absent, so a
/// client can treat presence as meaningful.
#[test]
fn the_envelope_omits_absent_detail() {
    let body = ErrorBody::new(ErrorCode::NotFound, "no pending notification with that id");
    assert_eq!(
        body.to_json(),
        r#"{"code":"not_found","message":"no pending notification with that id"}"#
    );
}

/// A moved gate carries the current pending gate, which is what lets a client re-render
/// without a second round-trip.
#[test]
fn gate_moved_carries_corrective_detail() {
    let body = ErrorBody::new(
        ErrorCode::GateMoved,
        "that call is no longer the pending gate",
    )
    .with_detail(serde_json::json!({ "call_id": "toolu_2" }));
    assert_eq!(
        body.to_json(),
        r#"{"code":"gate_moved","message":"that call is no longer the pending gate","detail":{"call_id":"toolu_2"}}"#
    );
    assert_eq!(body.code.status(), 409);
}

/// Codes serialize as snake_case, which is what a client matches on.
#[test]
fn codes_serialize_as_snake_case() {
    let json = serde_json::to_string(&ErrorCode::InvalidDecision).unwrap();
    assert_eq!(json, r#""invalid_decision""#);
}
