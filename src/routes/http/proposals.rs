//! `/proposals`: async config-edit proposals awaiting an orchestrator (docs/permissions.md).

use super::*;
use crate::protocol::answers::ProposalResolution;

/// List async config-edit proposals awaiting an orchestrator decision
/// (docs/permissions.md). The same array `/state` carries; a
/// dedicated GET lets a client poll just this without the rest of the snapshot.
pub(super) async fn list_proposals(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let body = serde_json::to_string(&shared.pending_proposals()).unwrap_or_else(|_| "[]".into());
    conn.with_status(200).with_body(body).halt()
}

/// Move a proposal to its terminal state by its `seq` (the `Proposed` entry's
/// id). Body: `{"state":"committed"|"cancelled","note":"…"}`. Committing folds
/// the proposed change into the effective config (and notifies the session for
/// scope/paths); cancelling is audit-only. Either way the proposal leaves the
/// pending queue. The orchestrator-above legislates here — the session can only
/// *file* a proposal, never resolve its own.
pub(super) async fn patch_proposal(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let Some(seq) = conn.param("seq").and_then(|s| s.parse::<u64>().ok()) else {
        return fail(
            conn,
            ErrorCode::BadRequest,
            "proposal seq must be an integer",
        );
    };
    let raw = conn.request_body_string().await.unwrap_or_default();
    let decision = match serde_json::from_str::<ProposalPatchBody>(&raw) {
        Ok(d) => d,
        Err(e) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("bad /proposals body: {e}"),
            );
        }
    };
    let commit = match decision.state.as_str() {
        "committed" => true,
        "cancelled" => false,
        other => {
            return fail(
                conn,
                ErrorCode::InvalidDecision,
                format!("state must be \"committed\" or \"cancelled\", got {other:?}"),
            );
        }
    };
    match shared.resolve_proposal_decision(seq, commit, decision.note.unwrap_or_default()) {
        Ok(true) => views::answer(
            conn,
            200,
            ProposalResolution {
                state: if commit {
                    EditState::Committed
                } else {
                    EditState::Cancelled
                },
            },
        ),
        Ok(false) => fail(
            conn,
            ErrorCode::NotFound,
            "no pending proposal with that seq",
        ),
        Err(e) => fail(conn, ErrorCode::Internal, format!("append failed: {e}")),
    }
}
