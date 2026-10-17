//! The listener-route verbs: the host's own routes at the socket root —
//! sessions, the inference catalog, attention, and the `/auth/*` credentials
//! (docs/control-api.md) — whatever prefix the client carries.

use super::*;

use super::transport::{json_field, read_2xx};
use trillium_client::Method;

fn catalog_query(
    cwd: &std::path::Path,
    config: Option<&std::path::Path>,
    max_age: Option<u64>,
    role: Option<ModelRole>,
) -> String {
    let mut query = trillium_client::url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("cwd", &cwd.to_string_lossy());
    if let Some(config) = config {
        query.append_pair("config", &config.to_string_lossy());
    }
    if let Some(max_age) = max_age {
        query.append_pair("max_age", &max_age.to_string());
    }
    if let Some(role) = role {
        query.append_pair("role", role.name());
    }
    query.finish()
}

impl Client {
    /// Create a session on a host (`POST /sessions`, docs/control-api.md) and
    /// return its id. A listener route, not a session's: it sits at the socket
    /// root regardless of this client's prefix, so the usual pairing is a
    /// prefix-less client for creation and [`at`](Self::at) for what follows.
    /// The request carries the caller's environment; it is written to the wire
    /// and to nothing else, and an error quotes the response, not the request.
    pub async fn create_session(&self, request: &CreateSession) -> Result<String> {
        let body = serde_json::to_string(request).context("encoding the session request")?;
        let response = self.send_at(Method::Post, "/sessions", Some(body)).await?;
        json_field(&response, "id")
    }

    /// Change one session's state on a host (`PATCH /sessions/:session`,
    /// docs/control-api.md) and return the state it ended in. The same transition
    /// as `DELETE /sessions/:session`, spelled as the change it is, so a client
    /// picks by whether it knows what it wants.
    ///
    /// A listener route, not a session's: it addresses the host's own root
    /// regardless of this client's prefix. The request carries the caller's
    /// environment; it is written to the wire and to nothing else, and an error
    /// quotes the response, not the request.
    pub async fn patch_session(&self, id: &str, request: &PatchSession) -> Result<SessionState> {
        let body = serde_json::to_string(request).context("encoding the session patch")?;
        let path = format!("/sessions/{id}");
        let response = self.send_at(Method::Patch, &path, Some(body)).await?;
        let state = serde_json::from_str::<serde_json::Value>(&response)
            .ok()
            .and_then(|value| value.get("state").cloned())
            .ok_or_else(|| anyhow!("no \"state\" in response: {response}"))?;
        serde_json::from_value(state).context("reading the patched state")
    }

    /// Set the running session `id`'s supervisor, or clear it with `None`
    /// (`PATCH /sessions/:session {"supervisor"}`, docs/permissions.md), and return
    /// the pointer as it now reads. A listener route, like
    /// [`patch_session`](Self::patch_session).
    pub async fn set_supervisor(
        &self,
        id: &str,
        supervisor: Option<&str>,
    ) -> Result<Option<String>> {
        let request = PatchSession::supervise(supervisor.map(str::to_string));
        let body = serde_json::to_string(&request).context("encoding the session patch")?;
        let path = format!("/sessions/{id}");
        let response = self.send_at(Method::Patch, &path, Some(body)).await?;
        let value = serde_json::from_str::<serde_json::Value>(&response)
            .ok()
            .and_then(|value| value.get("supervisor").cloned())
            .ok_or_else(|| anyhow!("no \"supervisor\" in response: {response}"))?;
        serde_json::from_value(value).context("reading the patched supervisor")
    }

    /// Resolve a proposed hosted session (`QUERY /sessions`, docs/control-api.md)
    /// without creating a directory or starting a runtime: the effective
    /// selections, where each came from, and the issues that would stop creation.
    /// A listener route, so it always addresses the socket root.
    pub async fn session_options(
        &self,
        request: &SessionOptionsRequest,
    ) -> Result<ResolvedOptions> {
        let body = serde_json::to_string(request).context("encoding the session options")?;
        let path = "/sessions";
        let body = self.send_at(Method::Query, path, Some(body)).await?;
        serde_json::from_str(&body).with_context(|| format!("decoding QUERY {path}"))
    }

    /// Every configured connection's stored model listing, with recent ranks for
    /// the project at `cwd` (`GET /inference/catalog`). Never waits on a provider:
    /// an entry's `status` says whether its copy is fresh, stale, missing, or
    /// failed, and a missing or stale one is being refreshed in the host
    /// (docs/model-catalog.md). `role` narrows the answer to what that role may
    /// choose; `None` is every model, which is what the spotter may choose.
    pub async fn catalog(
        &self,
        cwd: &std::path::Path,
        config: Option<&std::path::Path>,
        role: Option<ModelRole>,
    ) -> Result<Vec<ConnectionCatalog>> {
        #[derive(serde::Deserialize)]
        struct Catalog {
            connections: Vec<ConnectionCatalog>,
        }
        let query = catalog_query(cwd, config, None, role);
        let catalog: Catalog = self
            .get_root(&format!("/inference/catalog?{query}"))
            .await?;
        Ok(catalog.connections)
    }

    /// One connection's entry (`GET /inference/catalog/:connection`), refreshed
    /// and awaited by the host when its copy is older than `max_age` seconds
    /// (the host's 24-hour TTL when `None`; it floors the value at one minute).
    pub async fn catalog_connection(
        &self,
        connection: &str,
        cwd: &std::path::Path,
        config: Option<&std::path::Path>,
        max_age: Option<u64>,
        role: Option<ModelRole>,
    ) -> Result<ConnectionCatalog> {
        let name =
            percent_encoding::utf8_percent_encode(connection, percent_encoding::NON_ALPHANUMERIC);
        let query = catalog_query(cwd, config, max_age, role);
        self.get_root(&format!("/inference/catalog/{name}?{query}"))
            .await
    }

    /// The host's configured inference connections (`GET /inference`).
    pub async fn inference_inventory(&self) -> Result<Vec<ConnectionInventory>> {
        #[derive(serde::Deserialize)]
        struct Inventory {
            connections: Vec<ConnectionInventory>,
        }
        let inventory: Inventory = self.get_root("/inference").await?;
        Ok(inventory.connections)
    }

    /// The sessions a host holds (`GET /sessions`, docs/control-api.md), sorted
    /// by id, each with its title once one has been written.
    pub async fn sessions(&self) -> Result<Vec<ListedSession>> {
        self.get_root("/sessions").await
    }

    /// The dormant sessions under the working root `cwd`
    /// (`GET /sessions?state=archived&cwd=<root>`, docs/control-api.md), most
    /// recently active first. Rows with a `problem` are directories the host
    /// could not read as sessions; a session the host is running is not listed.
    pub async fn archived_sessions(&self, cwd: &std::path::Path) -> Result<Vec<ArchivedSession>> {
        let query = trillium_client::url::form_urlencoded::Serializer::new(String::new())
            .append_pair("state", "archived")
            .append_pair("cwd", &cwd.to_string_lossy())
            .finish();
        self.get_root(&format!("/sessions?{query}")).await
    }

    /// Subscribe to the host's session listing (`GET /sessions` with
    /// `Accept: text/event-stream`, docs/control-api.md). Each event carries the
    /// whole listing — the same array [`sessions`](Self::sessions) returns — so a
    /// client holds the latest frame and needs no cursor and no reconciliation.
    pub async fn session_events(&self) -> Result<trillium_client::sse::EventStream> {
        let path = "/sessions";
        self.http
            .get(path)
            .into_sse()
            .await
            .map_err(|e| anyhow!("opening the session listing stream at {path}: {e}"))
    }

    /// The ids of the sessions a host holds: [`sessions`](Self::sessions) without
    /// the titles.
    pub async fn list_sessions(&self) -> Result<Vec<String>> {
        Ok(self
            .sessions()
            .await?
            .into_iter()
            .map(|listing| listing.id)
            .collect())
    }

    /// End a hosted session (`DELETE /sessions/:id`, docs/control-api.md):
    /// answers once its teardown has run, with the sessions that ended with it.
    /// `404` for an id the host does not hold.
    pub async fn archive_session(&self, id: &str) -> Result<Vec<String>> {
        let body = self
            .send_at(Method::Delete, &format!("/sessions/{id}"), None)
            .await?;
        let answer: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        Ok(answer["ended_with"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Report whether a person is attending (`POST /attention`,
    /// docs/web-push.md). `Ok(false)` is a `404`: a lone session bind serves no
    /// attention table, so the caller stops reporting.
    pub(crate) async fn report_attention(
        &self,
        report: &crate::daemon::attention::AttentionReport,
    ) -> Result<bool> {
        let path = "/attention";
        let body = serde_json::to_string(report)?;
        let conn = self.exchange(Method::Post, path, Some(body)).await?;
        if conn.status().is_some_and(|s| s.code() == "404") {
            return Ok(false);
        }
        read_2xx(conn, Method::Post, path).await?;
        Ok(true)
    }

    /// A fresh bootstrap link from the daemon's TCP bind (`GET /auth/link`,
    /// docs/web-bind.md) — current TOTP code in the fragment. The `/auth/*`
    /// credential routes answer only over the Unix socket (host possession is the
    /// enrollment authority) and live beside `/v1`.
    pub async fn auth_link(&self) -> Result<String> {
        self.auth_field("/auth/link", "url").await
    }

    /// The authenticator-app enrollment URI (`GET /auth/otpauth`, docs/web-bind.md).
    /// Carries the TOTP secret itself — the caller renders it as a QR at the host
    /// terminal and nowhere else.
    pub async fn auth_otpauth(&self) -> Result<String> {
        self.auth_field("/auth/otpauth", "uri").await
    }

    /// The program bearer for the TCP bind (`GET /auth/bearer`, docs/web-bind.md).
    pub async fn auth_bearer(&self) -> Result<String> {
        self.auth_field("/auth/bearer", "bearer").await
    }

    /// GET a listener route and decode its JSON body.
    async fn get_root<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let body = self.send_at(Method::Get, path, None).await?;
        serde_json::from_str(&body).with_context(|| format!("decoding GET {path}"))
    }

    /// One string field of an `/auth/*` response, each a single-field object.
    async fn auth_field(&self, path: &str, field: &str) -> Result<String> {
        json_field(&self.send_at(Method::Get, path, None).await?, field)
    }
}
