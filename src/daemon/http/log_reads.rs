//! `/log`, `/tool_calls`, `/tools`: reads of the durable history and the session-constant
//! tool catalog (docs/control-api.md).

use super::*;

/// The durable history as a JSON array. Unbounded by default; `?since=<seq>`
/// resumes forward, `?tail=N` opens cold at the tail, `?before=<seq>&tail=N` pages
/// one window backward (docs/control-api.md). A window may be cut anywhere —
/// references that dangle across the cut resolve through the two routes below.
pub(super) async fn read_log_window(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    match LogSelect::parse(conn.querystring()) {
        Ok(select) => conn
            .with_status(200)
            .with_body(shared.log_json(&select))
            .halt(),
        Err(message) => fail(conn, ErrorCode::BadRequest, message),
    }
}

/// The same window as [`read_log_window`], rendered as `text/plain`
/// (`daemon/transcript.rs`, docs/control-api.md): at the conversation grain by
/// default, entry by entry with `grain=audit`, or one entry in full with `seq`. Every
/// line carries its handle, and the bodies a window leaves out resolve through the
/// zoom and the routes below.
pub(super) async fn read_transcript(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let query = match TranscriptQuery::parse(conn.querystring()) {
        Ok(query) => query,
        Err(message) => return fail(conn, ErrorCode::BadRequest, message),
    };
    let me = query.me.as_deref();
    let body = match query.seq {
        Some(handle) => match shared.transcript_zoom(handle, me) {
            Ok(text) => text,
            Err(message) => return fail(conn, ErrorCode::NotFound, message),
        },
        None => shared.transcript(&query.select, query.grain, me),
    };
    conn.with_response_header("content-type", "text/plain; charset=utf-8")
        .with_status(200)
        .with_body(body)
        .halt()
}

/// One entry by `seq` — the general form of reference resolution for a client
/// reading a window rather than the whole log (docs/control-api.md).
pub(super) async fn read_log_entry(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let Some(seq) = conn.param("seq").and_then(|s| s.parse::<u64>().ok()) else {
        return fail(conn, ErrorCode::BadRequest, "log seq must be an integer");
    };
    match shared.entry_json(seq) {
        Some(body) => conn.with_status(200).with_body(body).halt(),
        None => fail(conn, ErrorCode::NotFound, "no log entry with that seq"),
    }
}

/// One logged result image's bytes (docs/control-api.md): the dereference target
/// for the image references every entry view carries — views omit an image's
/// base64 `data` (entry_view.rs), and a client fetches the pixels here instead,
/// where they are HTTP-cached rather than re-shipped with every window or
/// replay. Raw bytes under the stored media type, so the URL works directly as
/// an `<img src>`.
pub(super) async fn read_result_image(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let Some(seq) = conn.param("seq").and_then(|s| s.parse::<u64>().ok()) else {
        return fail(conn, ErrorCode::BadRequest, "log seq must be an integer");
    };
    let Some(idx) = conn.param("idx").and_then(|s| s.parse::<usize>().ok()) else {
        return fail(
            conn,
            ErrorCode::BadRequest,
            "image index must be an integer",
        );
    };
    match shared.image_bytes(seq, idx) {
        Some((media_type, bytes)) => conn
            .with_response_header("content-type", media_type)
            .with_status(200)
            .with_body(bytes)
            .halt(),
        None => fail(conn, ErrorCode::NotFound, "no image at that seq and index"),
    }
}

/// One tool call by the `call_id` the log, the gate, and `tool_output` frames all
/// already carry. A call is embedded in an `Assistant` entry rather than being one,
/// which is why it needs an address of its own (docs/control-api.md).
pub(super) async fn read_tool_call(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let call_id = conn.param("call_id").unwrap_or_default().to_string();
    match shared.tool_call_json(&call_id) {
        Some(body) => conn.with_status(200).with_body(body).halt(),
        None => fail(conn, ErrorCode::NotFound, "no tool call with that id"),
    }
}

/// One tool's full definition, for any name the turn's `tools[]` can
/// carry — mounted providers and the harness's own loop-owned family
/// (`consolidate`, the process-monitor/inbox tools, the two triage tools)
/// alike (docs/control-api.md): the description, input_schema,
/// annotations, title, and which server mounted it (`null` for a
/// loop-owned tool). A lazy per-tool fetch, not a table pushed at
/// session start. `:name` accepts the dotted operator spelling
/// (`ferritin.get`) as well as the wire form (`ferritin__get`);
/// `wire_tool_name` is a no-op on an already-wire name.
pub(super) async fn read_tool_definition(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let Some(raw_name) = conn.param("name").map(str::to_string) else {
        return fail(conn, ErrorCode::BadRequest, "missing tool name");
    };
    let wire_name = crate::config::wire_tool_name(raw_name);
    match shared
        .tool_entry(&wire_name)
        .and_then(|entry| serde_json::to_string(&entry).ok())
    {
        Some(body) => conn.with_status(200).with_body(body).halt(),
        None => fail(
            conn,
            ErrorCode::NotFound,
            "no tool with that name in tools[]",
        ),
    }
}
