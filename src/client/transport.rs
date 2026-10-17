//! How every verb reaches the socket: one request shape and one reading of
//! its response (docs/control-api.md).

use super::*;

use trillium_client::Method;

/// What a wait route answered (docs/control-api.md).
pub struct Waited {
    /// The `/state` body.
    pub body: String,
    /// `false` when the host's cap expired and `body` is only where the session was then.
    pub settled: bool,
}

impl Client {
    /// Send `method` to `path` exactly as given — no prefix, no version — with a
    /// JSON body when there is one, and hand back the finished conn.
    pub(super) async fn exchange(
        &self,
        method: Method,
        path: &str,
        body: Option<String>,
    ) -> Result<trillium_client::Conn> {
        let conn = self.http.build_conn(method, path);
        let conn = match body {
            Some(body) => conn
                .with_request_header("content-type", "application/json; charset=utf-8")
                .with_body(body),
            None => conn,
        };
        conn.await
            .with_context(|| format!("{method} {path} on the control socket"))
    }

    /// [`exchange`](Self::exchange), returning the response body on a 2xx and an
    /// error carrying the status and body otherwise. The listener routes, which sit
    /// at the socket root whatever this client's prefix, are sent through this.
    pub(super) async fn send_at(
        &self,
        method: Method,
        path: &str,
        body: Option<String>,
    ) -> Result<String> {
        let conn = self.exchange(method, path, body).await?;
        read_2xx(conn, method, path).await
    }

    /// A wait route's answer: the `/state` body and whether the host says the session
    /// settled or its cap expired (`efference-settled`, docs/control-api.md). A response
    /// without the header counts as settled, as a host that predates it always meant.
    pub(super) async fn wait_at(
        &self,
        method: Method,
        path: &str,
        body: Option<String>,
    ) -> Result<Waited> {
        let path = self.versioned(path);
        let conn = self.exchange(method, &path, body).await?;
        let settled = conn
            .response_headers()
            .get_str(crate::daemon::SETTLED_HEADER)
            .is_none_or(|value| value != "false");
        let body = read_2xx(conn, method, &path).await?;
        Ok(Waited { body, settled })
    }

    /// GET a session route under this client's prefix and `/v1`.
    pub(super) async fn get(&self, path: &str) -> Result<String> {
        self.send_at(Method::Get, &self.versioned(path), None).await
    }

    /// POST a JSON body to a session route.
    pub(super) async fn post(&self, path: &str, body: String) -> Result<String> {
        self.send_at(Method::Post, &self.versioned(path), Some(body))
            .await
    }

    /// PATCH a JSON body to a session route.
    pub(super) async fn patch(&self, path: &str, body: String) -> Result<String> {
        self.send_at(Method::Patch, &self.versioned(path), Some(body))
            .await
    }

    /// PUT a JSON body to a session route.
    pub(super) async fn put(&self, path: &str, body: String) -> Result<String> {
        self.send_at(Method::Put, &self.versioned(path), Some(body))
            .await
    }

    /// DELETE a session route.
    pub(super) async fn delete(&self, path: &str) -> Result<String> {
        self.send_at(Method::Delete, &self.versioned(path), None)
            .await
    }

    /// GET a session route, returning the raw response bytes on a 2xx — for the
    /// one non-JSON resource (image bytes); everything else reads through
    /// [`get`](Self::get).
    pub(super) async fn get_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let path = self.versioned(path);
        let mut conn = self.exchange(Method::Get, &path, None).await?;
        if conn.status().is_some_and(|s| s.is_success()) {
            conn.response_body()
                .read_bytes()
                .await
                .with_context(|| format!("reading the response to GET {path}"))
        } else {
            Err(anyhow!("GET {path} → {}", status_code(&conn)))
        }
    }

    /// Join the base prefix and the API version onto a verb's path
    /// (docs/control-api.md): every session route is served under `/v1`, and
    /// prefixing here — the one place every session request passes through — keeps
    /// the verbs reading like the API they document.
    pub(super) fn versioned(&self, path: &str) -> String {
        format!("{}/v1{path}", self.prefix)
    }
}

/// Read a finished client conn's body, mapping a non-2xx status into an error that
/// carries the daemon's own JSON error message (so a 404 "no pending approval"
/// surfaces verbatim at the CLI).
pub(super) async fn read_2xx(
    mut conn: trillium_client::Conn,
    method: Method,
    path: &str,
) -> Result<String> {
    let ok = conn.status().is_some_and(|s| s.is_success());
    let code = status_code(&conn);
    let body = conn
        .response_body()
        .read_string()
        .await
        .with_context(|| format!("reading the response to {method} {path}"))?;
    if ok {
        Ok(body)
    } else {
        Err(anyhow!("{method} {path} → {code}: {body}"))
    }
}

/// Pull one string field out of a JSON body — a single-field response, or a
/// decline's `message`.
pub(super) fn json_field(body: &str, field: &str) -> Result<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get(field).and_then(|f| f.as_str()).map(String::from))
        .ok_or_else(|| anyhow!("no {field:?} in response: {body}"))
}

/// A finished conn's status code, or `no status` when no response arrived.
fn status_code(conn: &trillium_client::Conn) -> String {
    conn.status()
        .map(|s| s.code().to_string())
        .unwrap_or_else(|| "no status".into())
}
