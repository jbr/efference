//! `/attachments`: a composer's uploads (docs/inbox.md, docs/control-api.md).
//!
//! A client stages a file by uploading it here and names it in the `POST /turns` that sends it.
//! Upload happens at staging time rather than at send, so a client learns what the daemon made of
//! the file (the same triage signals the model will read) while it is still removable, and a
//! recalled turn hands back names that are still there to re-stage.

use super::*;

/// The most one upload may carry. Above trillium's 10 MB default because a screenshot on a
/// high-density display, or a log file, can pass it; bounded because the bytes are held in memory
/// while they are read.
const UPLOAD_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Store the request body as an attachment. `?name=` suggests the file name — a dragged file's
/// own, or one the client makes for a paste — and the store makes it unique rather than
/// overwriting. 201 with the arrival: `{name, path, kind}`.
///
/// A failure logs at `warn` naming its stage (the store, the body read, the write), with the
/// declared length beside it: a client sees only the answer, and an upload that never reached
/// the handler leaves no line at all — which is itself the stage. A stored upload logs at
/// `debug`, under the host's output discipline (`serve.rs`).
pub(super) async fn upload(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let name = trillium_client::url::form_urlencoded::parse(conn.querystring().as_bytes())
        .find(|(key, _)| key == "name")
        .map_or_else(|| "attachment".to_string(), |(_, value)| value.into_owned());
    let declared = conn
        .request_headers()
        .get_str(trillium::KnownHeaderName::ContentLength)
        .unwrap_or("none")
        .to_string();
    let Some(store) = shared.composer_store() else {
        log::warn!("upload {name:?} (content-length {declared}): refused, no attachment store");
        return fail(
            conn,
            ErrorCode::NotFound,
            "this session has no attachment store",
        );
    };
    let bytes = match conn
        .request_body()
        .with_max_len(UPLOAD_MAX_BYTES)
        .read_bytes()
        .await
    {
        Ok(bytes) => bytes,
        Err(e) => {
            log::warn!(
                "upload {name:?} (content-length {declared}) to {}: reading the body failed: {e}",
                store.dir().display()
            );
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("reading the upload: {e}"),
            );
        }
    };
    match store.put(&name, &bytes) {
        Ok(arrival) => {
            log::debug!("upload stored {} ({} bytes)", arrival.path, bytes.len());
            conn.with_status(201)
                .with_body(serde_json::to_string(&arrival).unwrap_or_default())
                .halt()
        }
        Err(e) => {
            log::warn!(
                "upload {name:?} ({} bytes) to {}: storing failed: {e}",
                bytes.len(),
                store.dir().display()
            );
            fail(
                conn,
                ErrorCode::Internal,
                format!("storing the upload: {e}"),
            )
        }
    }
}

/// An uploaded file's bytes, under its sniffed media type. Not cacheable: unlike a log entry's
/// image (`GET /log/:seq/images/:idx`), a stored file can be removed.
pub(super) async fn read(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let name = name_param(&conn);
    let Some(path) = shared
        .composer_store()
        .and_then(|store| store.path(&name).ok())
    else {
        return fail(conn, ErrorCode::NotFound, "no attachment by that name");
    };
    match std::fs::read(&path) {
        Ok(bytes) => conn
            .with_response_header("content-type", crate::attachments::media_type(&path))
            .with_response_header("cache-control", "no-store")
            .with_status(200)
            .with_body(bytes)
            .halt(),
        Err(e) => fail(
            conn,
            ErrorCode::NotFound,
            format!("reading the attachment: {e}"),
        ),
    }
}

/// Remove a staged upload the operator took back. A file already sent stays: the turn that
/// carried it names its path, and the model may not have read it yet — so this is for the
/// composer's own staging, and a client removes only what it has not sent.
pub(super) async fn remove(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let name = name_param(&conn);
    match shared.composer_store().map(|store| store.remove(&name)) {
        Some(Ok(())) => conn.with_status(204).halt(),
        _ => fail(conn, ErrorCode::NotFound, "no attachment by that name"),
    }
}

/// The `:name` capture, percent-decoded: a name is whatever the upload was stored under, spaces
/// and all, and the router hands the capture over still encoded.
fn name_param(conn: &Conn) -> String {
    let raw = conn.param("name").unwrap_or_default();
    percent_encoding::percent_decode_str(raw)
        .decode_utf8_lossy()
        .into_owned()
}
