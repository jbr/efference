//! The remembered launch locations (docs/control-api.md): the index a client
//! offers as quick-start locations, and the route that forgets one.

use super::{ErrorCode, fail, host_of, missing_host};
use crate::remembered::Project;
use std::path::PathBuf;
use trillium::Conn;

/// `GET /projects`: the directories sessions have been started in, most recently
/// used first.
///
/// Availability is checked here rather than stored, and **omitting an entry is
/// not deleting it**: a directory on an unmounted volume is filtered out of this
/// answer and is on the next one when the volume is back. The check stats the
/// filesystem, which blocks on an unresponsive mount, so it runs off the
/// executor — an index of a handful of directories is otherwise a request whose
/// latency is whatever the slowest volume says.
///
/// Creation re-validates what this filter checked: a volume can unmount between
/// the two, and `POST /sessions` answers for itself.
pub(super) async fn list_projects(conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let stored = host.remembered.recent();
    let available = match tokio::task::spawn_blocking(move || {
        stored
            .into_iter()
            .filter(Project::available)
            .collect::<Vec<Project>>()
    })
    .await
    {
        Ok(available) => available,
        // A filter that did not run is not a reason to report no remembered
        // locations: the entries are stored either way, and one that turns out
        // to be gone is refused by creation, with its own reason, if a client
        // takes it up.
        Err(_) => host.remembered.recent(),
    };
    let projects: Vec<serde_json::Value> = available
        .iter()
        .map(|project| {
            serde_json::json!({
                // What a picker shows without being given a name; there is no
                // route to set one.
                "label": project.label(),
                // The creation request this entry stands for, spelled as
                // `POST /sessions` spells it: `config` absent asks creation to
                // probe, which is what a launch without a named configuration did.
                "cwd": project.cwd.to_string_lossy(),
                "config": project.config.as_ref().map(|c| c.to_string_lossy().into_owned()),
                // The identity the entry is stored under, and what
                // `DELETE /projects` takes. Reported rather than recomputed
                // because recomputing re-probes the filesystem.
                "config_key": project.config_key,
                "last_used_at": project.last_used_at,
            })
        })
        .collect();
    conn.with_status(200)
        .with_body(serde_json::json!({ "projects": projects }).to_string())
        .halt()
}

/// `DELETE /projects?cwd=<root>&config_key=<key>`: forget one entry, by the
/// identity the index reported.
///
/// Answers `404` for an identity that is not remembered, which a client reads as
/// "already gone" — the state it asked for. Deletes the remembered entry and
/// nothing else: no file, no directory, and no session that ran there. Not
/// listing an entry is not forgetting it, so an unavailable location stays
/// remembered and comes back on its own.
pub(super) async fn forget_project(conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let query = conn.querystring();
    let (Some(cwd), Some(config_key)) = (
        crate::daemon::http::query_value(query, "cwd"),
        crate::daemon::http::query_value(query, "config_key"),
    ) else {
        return fail(
            conn,
            ErrorCode::BadRequest,
            "forget takes the cwd and config_key the index reported, \
             e.g. ?cwd=/Users/jbr/code/efference&config_key=builtin-default",
        );
    };
    let cwd = PathBuf::from(cwd);
    if !host.remembered.forget(&cwd, &config_key) {
        return fail(
            conn,
            ErrorCode::NotFound,
            format!(
                "no remembered location at {}; read GET /projects for the current list",
                cwd.display()
            ),
        );
    }
    conn.with_status(200)
        .with_body(
            serde_json::json!({ "forgotten": { "cwd": cwd.to_string_lossy(), "config_key": config_key } })
                .to_string(),
        )
        .halt()
}
