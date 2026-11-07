//! The model catalog routes (docs/control-api.md, docs/model-catalog.md):
//! every connection's normalized listing with recent picks folded in.

use super::{ErrorCode, Host, fail, host_of, missing_host};
use crate::inference::catalog::{ModelRole, Recent};
use crate::protocol::answers::CatalogList;
use crate::routes::views;
use std::time::Duration;
use trillium::Conn;
use trillium_router::RouterConnExt;

/// Recent picks in the project scope named by this request.
fn recents(host: &Host, query: &str) -> Vec<(String, Recent)> {
    let param = |name: &str| crate::routes::http::query_value(query, name);
    let Some(cwd) = param("cwd") else {
        return host.catalog_recents(None);
    };
    let scope = crate::config::history::Scope::for_request(
        &crate::platform::home::canonical_here(std::path::Path::new(&cwd)),
        param("config").as_deref().map(std::path::Path::new),
    );
    host.catalog_recents(Some(&scope))
}

/// The role a request names, if any: `Ok(None)` for no `role` parameter, an
/// error message for one that is not a role.
fn role_of(query: &str) -> Result<Option<ModelRole>, String> {
    match crate::routes::http::query_value(query, "role") {
        None => Ok(None),
        Some(name) => ModelRole::from_name(&name).map(Some).ok_or_else(|| {
            format!("role is `main`, `triage`, or `spotter`, not {name:?}, e.g. ?role=spotter")
        }),
    }
}

/// `GET /inference/catalog?cwd=<root>&config=<path>&role=<role>`: every configured
/// connection's stored listing, its age, and its status. Never waits on a
/// provider: a missing or stale copy starts a background refresh and is served
/// as it stands. `cwd` (+ optional `config`) scopes the recent ranks; without
/// `cwd` ranks use the system-wide aggregate. With `role` (`main`, `triage`, or `spotter`) the
/// answer holds only what that role may choose: no decision models for a role
/// that runs turns, and no connection that serves none. Without it nothing is
/// filtered.
pub(crate) async fn catalog_all(conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let role = match role_of(conn.querystring()) {
        Ok(role) => role,
        Err(message) => return fail(conn, ErrorCode::BadRequest, message),
    };
    let picks = recents(&host, conn.querystring());
    let mut connections = Vec::new();
    for row in host.inference.inventory() {
        if role.is_some_and(|role| !role.accepts_provider(row.provider)) {
            continue;
        }
        let listed = host.catalog_entry(&row, &picks, None).await;
        connections.push(match role {
            Some(role) => match listed.for_role(role) {
                Some(listed) => listed,
                None => continue,
            },
            None => listed,
        });
    }
    views::answer(conn, 200, CatalogList { connections })
}

/// `GET /inference/catalog/:connection?max_age=<secs>&cwd=&config=&role=`: one
/// connection's entry, refreshed and awaited when its copy is older than
/// `max_age` (default the catalog TTL, floor one minute). A failed read answers
/// with the old copy, if any, and `status: "failed"`. `role` filters as on the
/// whole catalog; naming a role the connection cannot serve at all is a 400.
pub(crate) async fn catalog_one(conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let name = conn.param("connection").unwrap_or_default().to_string();
    let Some(row) = host
        .inference
        .inventory()
        .into_iter()
        .find(|row| row.name == name)
    else {
        return fail(
            conn,
            ErrorCode::NotFound,
            format!("no connection named {name:?}"),
        );
    };
    let query = conn.querystring();
    let role = match role_of(query) {
        Ok(role) => role,
        Err(message) => return fail(conn, ErrorCode::BadRequest, message),
    };
    if let Some(role) = role
        && !role.accepts_provider(row.provider)
    {
        return fail(
            conn,
            ErrorCode::BadRequest,
            format!(
                "connection {name:?} serves no turns, so it cannot back the {} role",
                role.name()
            ),
        );
    }
    let max_age = match crate::routes::http::query_value(query, "max_age") {
        Some(text) => match text.parse::<u64>() {
            Ok(secs) => Duration::from_secs(secs),
            Err(_) => {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    "max_age is whole seconds, e.g. ?max_age=300",
                );
            }
        },
        None => crate::inference::catalog::CATALOG_TTL,
    };
    let picks = recents(&host, query);
    let mut answer = host.catalog_entry(&row, &picks, Some(max_age)).await;
    if let Some(role) = role {
        answer.retain_for_role(role);
    }
    conn.with_status(200)
        .with_body(serde_json::to_string(&answer).unwrap_or_default())
        .halt()
}
