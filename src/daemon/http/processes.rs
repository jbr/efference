//! `/processes`: the tool-spawned process resource (docs/supervisor.md, docs/control-api.md).

use super::*;

/// The process resource (docs/supervisor.md, docs/control-api.md): every
/// running tool-spawned process — foreground and background share the one
/// namespace, `:id` is the bare spawn-minted ordinal. The listing is the
/// same representation the `processes` state topic carries.
pub(super) async fn list_processes(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let body = serde_json::to_string(&shared.process_states()).unwrap_or_else(|_| "[]".into());
    conn.with_status(200).with_body(body).halt()
}

/// One process's record — running or exited, live registry or log fold
/// (docs/supervisor.md): once a handle is minted it stays addressable at
/// one URL for the life of the session, including in a recorded session
/// whose daemon is long gone.
pub(super) async fn read_process(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let Some(id) = conn.param("id").and_then(|s| s.parse::<u64>().ok()) else {
        return fail(conn, ErrorCode::BadRequest, "process id must be an integer");
    };
    match shared.process_record(id).map(|p| serde_json::to_string(&p)) {
        Some(Ok(body)) => conn.with_status(200).with_body(body).halt(),
        _ => fail(conn, ErrorCode::NotFound, "no process with that id"),
    }
}

/// A cursor read of one process's retained output. `?since=<cursor>` resumes
/// after a previous read; with no cursor it returns everything retained — the
/// seed a cold-opened client renders before its first `process_output` delta
/// (docs/control-api.md). Works on exited processes too: the capture outlives
/// the exit, unlike the listing entry.
pub(super) async fn read_process_output(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let Some(id) = conn.param("id").and_then(|s| s.parse::<u64>().ok()) else {
        return fail(conn, ErrorCode::BadRequest, "process id must be an integer");
    };
    let since = match parse_u64_param(conn.querystring(), "since") {
        Ok(since) => since,
        Err(message) => return fail(conn, ErrorCode::BadRequest, message),
    };
    match shared.process_output_read(id, since) {
        Some(read) => {
            let body = serde_json::json!({
                "id": id,
                "output": read.content,
                "cursor": read.cursor,
                "lost": read.lost,
                "running": read.running,
            });
            conn.with_status(200).with_body(body.to_string()).halt()
        }
        None => fail(conn, ErrorCode::NotFound, "no process with that id"),
    }
}

/// Set a process's properties. `{"muted": …}` applies synchronously (the
/// supervisor's dismiss-a-live-process, and its inverse). `{"background":
/// true}` on the foreground entry is the foreground→background handoff
/// (docs/supervisor.md): answered 202, because the flip lands only when the
/// detach wake reaches the running tool's race — on a process that is
/// already background it is an idempotent no-op. An optional `note` rides
/// the handoff as the next user message. `background: false` has no
/// transition to name.
pub(super) async fn patch_process(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let Some(id) = conn.param("id").and_then(|s| s.parse::<u64>().ok()) else {
        return fail(conn, ErrorCode::BadRequest, "process id must be an integer");
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    let patch = match serde_json::from_str::<ProcessPatchBody>(&body) {
        Ok(p) => p,
        Err(e) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("bad /processes body: {e}"),
            );
        }
    };
    if patch.background == Some(false) {
        return fail(
            conn,
            ErrorCode::BadRequest,
            "a background process cannot be foregrounded",
        );
    }
    if patch.note.is_some() && patch.background != Some(true) {
        return fail(
            conn,
            ErrorCode::BadRequest,
            "note accompanies a background:true handoff",
        );
    }
    let Some(current) = shared.running_process(id) else {
        return fail(conn, ErrorCode::NotFound, "no running process with that id");
    };
    if let Some(muted) = patch.muted {
        shared.set_process_muted(id, muted);
    }
    // Only a foreground entry has anywhere to move, and the id in the path
    // is what says which entry.
    let detach = patch.background == Some(true) && !current.background;
    if detach {
        shared.request_detach(patch.note);
    }
    let representation = shared
        .running_process(id)
        .and_then(|p| serde_json::to_string(&p).ok())
        .unwrap_or_else(|| "null".into());
    conn.with_status(if detach { 202 } else { 200 })
        .with_body(representation)
        .halt()
}

/// Kill a running process: `killpg` its group; the drain task then sees EOF
/// and fires the exit notification (docs/supervisor.md).
pub(super) async fn kill_process(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let Some(id) = conn.param("id").and_then(|s| s.parse::<u64>().ok()) else {
        return fail(conn, ErrorCode::BadRequest, "process id must be an integer");
    };
    if shared.kill_process(id) {
        conn.with_status(200)
            .with_body(r#"{"status":"killed"}"#)
            .halt()
    } else {
        fail(conn, ErrorCode::NotFound, "no running process with that id")
    }
}
