//! `POST /sessions`: minting a session (docs/control-api.md).
//!
//! The counterpart of [`resume`](super::resume), and the same shape of work: the
//! caller supplies what a log cannot hold, the host prepares a session and starts
//! it, and the id comes back. Creation mints; resume reopens. The two differ most in
//! what they record — a launch and an explicit model pick in history, or nothing —
//! and in what they read: every option here is the caller's to choose, where a
//! resumed session has none.

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

impl Host {
    /// Create a session on this host: the path behind `POST /sessions`
    /// (docs/control-api.md) and the in-session `sessions__create` tool
    /// (docs/tools.md). `lineage` names the calling session as creator when a
    /// session commissioned this one, and the predecessor for a `hand_off`;
    /// default for a human root. Returns the minted id, or the reason nothing was
    /// minted.
    pub async fn create(
        self: &Arc<Self>,
        body: CreateSession,
        lineage: crate::env::Lineage,
    ) -> std::result::Result<String, String> {
        // The session base is what an unnamed id must be free in, on disk and in
        // the live map — a dead session's directory still holds its name
        // (docs/session-title.md).
        let base = body.cwd.join(SESSION_BASE);
        let id = body.name.unwrap_or_else(|| {
            slug::mint(|id| self.session(id).is_some() || base.join(id).exists())
        });
        if !super::tcp::plain_id(&id) {
            return Err(format!(
                "session name {id:?} is not a single plain path segment"
            ));
        }
        if self.session(&id).is_some() {
            return Err(format!("session {id:?} is already hosted"));
        }
        if !body.cwd.is_absolute() {
            return Err(format!("cwd must be absolute: {}", body.cwd.display()));
        }
        // An environment the caller supplied is the base, and nothing is run. An
        // omitted one is established here, for this session's own directory, from
        // the user-wide `env_hook` or the host environment (docs/env.md).
        let base_env = match body.env {
            Some(env) => match crate::env_table::EnvSnapshot::new(env) {
                Ok(base) => base,
                Err(e) => return Err(format!("env: {e}")),
            },
            None => {
                // Host shutdown cancels the establishment, which fails the
                // creation before an id is minted. Nothing else races it: a
                // client that disconnects does not cancel this work, so an
                // accepted creation completes and mints its session even with no
                // reader on the response.
                let abort = self.swansong.shutting_down();
                match crate::env_table::environment_at(
                    &body.cwd,
                    self.inference.env_hook(),
                    &self.host_env,
                    abort,
                )
                .await
                {
                    Ok(base) => base,
                    Err(e) => return Err(format!("env: {e}")),
                }
            }
        };
        // The same resolution preview reports, then credentials for the roles it
        // selected. Project effort is boot's to apply, so it is not resolved here.
        // History is resolved here: the omitted branch defaults to past picks
        // in this working root, and the write below records an explicit pick.
        let options = body.options.clone();
        let scope = crate::history::Scope::for_request(&body.cwd, body.config.as_deref());
        let resolved = self.inference.resolve_options(
            &body.options,
            None,
            Some((self.inference.history(), &scope)),
        );
        // Whether the main model was an explicit human pick: only those are
        // recorded, never an inherited default, so the default cannot reinforce
        // itself. Captured before `prepare` consumes the resolution.
        let main_explicit = matches!(
            resolved.effective.main.source,
            Some(crate::inference::SelectionSource::Request)
        );
        let inference = match self.inference.prepare(resolved) {
            Ok(inference) => inference,
            Err(error) => return Err(error.to_string()),
        };
        // Remember the pick this creation made, for the omitted-branch default
        // and the recent menus. Built before the request moves the inference
        // away; recorded below, after the session starts, so only sessions that
        // actually started are remembered. Best-effort either way: the store
        // never fails a creation.
        let pick = main_explicit.then(|| {
            let (selection, _) = inference.main();
            crate::history::Pick {
                at: crate::history::now(),
                cwd: scope.cwd.to_string_lossy().into_owned(),
                config_key: scope.config_key.clone(),
                connection: selection.connection.clone(),
                provider: selection.provider.to_string(),
                model: selection.model.clone(),
            }
        });
        // Where this session starts, for the index the launch picker reads. Every
        // creation records one — a web launch, a session's own `sessions__create`,
        // a `hand_off` in its predecessor's directory — because they are one
        // path here and a launch is what puts a directory on the list. Built
        // before the request takes `body.config`, which is also what keeps the row
        // naming the template configuration rather than the session's own copy;
        // `config` is the spelling, so the index can hand a client back exactly
        // what to create with. Recorded below, with the pick.
        let launch = crate::remembered::Project {
            last_used_at: crate::history::now(),
            cwd: scope.cwd.clone(),
            config_key: scope.config_key.clone(),
            config: body.config.clone(),
        };
        let request = crate::boot::SessionRequest {
            session_dir: body.cwd.join(SESSION_BASE).join(&id),
            cwd: body.cwd,
            base_env,
            config: body.config,
            inference,
            temperature: body.options.temperature,
            effort: body.options.effort,
            upstream: body.upstream,
            autonomous: body.autonomous,
            reauth: body.reauth,
            keep_scratch: body.keep_scratch,
            // The new session's toolbox mounts the `sessions` provider against
            // this host, with the new session itself as the caller (docs/tools.md).
            host: Some(Arc::clone(self)),
            // The creator and predecessor reach the identity line
            // (docs/context.md); `adopt_with` below records the same values as
            // control state.
            lineage: lineage.clone(),
        };
        let boot = match crate::boot::prepare(request).await {
            Ok(boot) => boot,
            Err(e) => return Err(format!("could not prepare session: {e}")),
        };
        let runtime = SessionRuntime::start(boot, self.swansong());
        // A name another create took while this one awaited the hook and
        // preparation: the loser is shut down inside `adopt_with`, so nothing
        // here records options or a model pick for a session that will not exist.
        self.adopt_with(id.clone(), runtime, lineage.created_by, body.detached)?;
        self.record_options(&id, options);
        // Remember the pick this creation made, for the omitted-branch default
        // and the recent menus, and the directory it started in, for the index.
        // Both after the session exists, so neither remembers a creation that
        // did not happen; best-effort, so neither store fails one that did.
        if let Some(pick) = pick.as_ref() {
            self.inference.history().record(pick);
        }
        self.remembered.record(&launch);
        Ok(id)
    }
}

/// Body of `POST /sessions` (docs/control-api.md): a `SessionRequest` as a
/// client spells it. One type on both ends — the control client serializes it,
/// the host deserializes it — so a new session knob is one field here. `env` is
/// the session's complete base environment (docs/env.md); it is applied and
/// dropped, never logged or echoed, which is why the type has no Debug.
///
/// `env` is optional because an omitted one is a *different request* from an
/// empty one. Supplied, it is the base and nothing runs; omitted, the base is
/// established for the session's directory from the user-wide `env_hook`, or
/// from the host's own environment when none is configured (docs/env.md). An
/// empty map is an explicit empty environment, so `{}` and omitted are different
/// requests.
#[derive(Serialize, Deserialize)]
pub struct CreateSession {
    /// The session id; a minted adjective-noun pair when absent
    /// (docs/session-title.md). A single plain path segment, as the gate's
    /// `plain_id` requires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The session's working root. Absolute, always sent.
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    /// The editable selections, spelled exactly as `QUERY /sessions` accepts
    /// them and resolved by the same flow (docs/control-api.md). Flattened, so
    /// the two routes cannot drift and a new option is one field in one place.
    #[serde(flatten)]
    pub options: SessionOptions,
    #[serde(default)]
    pub upstream: Vec<String>,
    #[serde(default)]
    pub autonomous: bool,
    #[serde(default)]
    pub reauth: Vec<String>,
    #[serde(default)]
    pub keep_scratch: bool,
    /// Outlive the session that creates this one (docs/tools.md). Meaningful only
    /// for a session-driven `create`: a human root has no creator to end with.
    #[serde(default)]
    pub detached: bool,
    /// The session's complete base environment, when the caller supplies one
    /// (docs/env.md). `None` asks the host to establish it for `cwd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

/// `POST /sessions`: prepare and start a session from the body. Mounted on every
/// host listener, so a network caller reaches this too.
///
/// Preparation reports to this process's stderr and may open a browser for an MCP
/// authorization, on the host — which a network caller cannot see or answer
/// (docs/control-api.md).
pub(super) async fn create_session(mut conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    let body: CreateSession = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(error) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("invalid body: {error}"),
            );
        }
    };
    // A human root: no session commissioned it, so it has no `created_by`
    // (docs/tools.md).
    match host.create(body, crate::env::Lineage::default()).await {
        Ok(id) => conn
            .with_status(201)
            .with_body(serde_json::json!({ "id": id }).to_string())
            .halt(),
        Err(reason) => fail(conn, ErrorCode::BadRequest, reason),
    }
}
