//! `/title`: an operator's title for the session (docs/session-title.md).

use super::*;

#[derive(Deserialize)]
struct TitleBody {
    title: String,
}

/// Set the session's title by hand: one `Title` entry with `author: operator`,
/// and the `session_identity` topic moves with it. The title peek reads the new
/// text as the standing title at its next ask. Whitespace-only text is refused,
/// as is a line over the peek's own cap, so the two authors are held to the
/// same shape.
pub(super) async fn put_title(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    let body: TitleBody = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(e) => return fail(conn, ErrorCode::BadRequest, format!("bad /title body: {e}")),
    };
    let text = body.title.trim();
    if text.is_empty() {
        return fail(conn, ErrorCode::BadRequest, "title must not be empty");
    }
    if text.chars().count() > crate::distill::TITLE_MAX_CHARS {
        return fail(
            conn,
            ErrorCode::BadRequest,
            format!(
                "title must be at most {} characters",
                crate::distill::TITLE_MAX_CHARS
            ),
        );
    }
    let title = crate::log::Title {
        text: text.to_string(),
        author: crate::log::TitleAuthor::Operator,
    };
    match shared.set_title(title) {
        Ok(Some(_)) => conn
            .with_status(200)
            .with_body(shared.session_identity_json().to_string())
            .halt(),
        Ok(None) => fail(
            conn,
            ErrorCode::BadRequest,
            "this session has no identity to title (no daemon booted it)",
        ),
        Err(e) => fail(
            conn,
            ErrorCode::Internal,
            format!("could not record the title: {e}"),
        ),
    }
}
