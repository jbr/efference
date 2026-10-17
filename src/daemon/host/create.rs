//! `POST /sessions`: minting a session (docs/control-api.md).
//!
//! The counterpart of [`resume`](super::resume), and the same shape of work: the
//! caller supplies what a log cannot hold, the host prepares a session and starts
//! it, and the id comes back. Creation mints; resume reopens. The two differ most in
//! what they record — a launch and an explicit model pick in history, or nothing —
//! and in what they read: every option here is the caller's to choose, where a
//! resumed session has none.

use super::preparation::authorization_for;
use super::*;
use crate::daemon::starting::{Stage, Starting, StartingParts};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

impl Host {
    /// Create a session on this host: the path behind `POST /sessions`
    /// (docs/control-api.md) and the in-session `agents__create` tool
    /// (docs/tools.md). `lineage` names the calling session as creator when a
    /// session commissioned this one, and the predecessor for a `hand_off`;
    /// default for a human root.
    ///
    /// Returns once the id is reserved and the session is in the map as starting:
    /// what can be refused from the request alone — the cwd, the name, a supplied
    /// environment, the role selections — is refused here, with nothing minted. The
    /// environment and preparation run after, in the background, and how they end is
    /// [`Starting::finished`](crate::daemon::starting::Starting::finished).
    pub(crate) fn create(
        self: &Arc<Self>,
        mut body: CreateSession,
        lineage: crate::env::Lineage,
    ) -> std::result::Result<Arc<Starting>, String> {
        if !body.cwd.is_absolute() {
            return Err(format!("cwd must be absolute: {}", body.cwd.display()));
        }
        // The session is rooted at its project and works in the directory the client
        // sent: they differ when that directory is inside a worktree
        // (docs/worktrees.md).
        let project = crate::worktree::project_of(&body.cwd).unwrap_or_else(|| body.cwd.clone());
        let directories = crate::policy::Directories::new(project.clone(), body.cwd.clone());
        // An environment the caller supplied is the base, and nothing is run.
        let supplied_env = match body.env.take() {
            Some(env) => {
                Some(crate::env_table::EnvSnapshot::new(env).map_err(|e| format!("env: {e}"))?)
            }
            None => None,
        };
        let plan = self.plan_creation(&body, &project)?;
        // The session base is what an unnamed id must be free in, on disk and in
        // the live map — a dead session's directory still holds its name
        // (docs/session-title.md).
        let base = project.join(SESSION_BASE);
        // The id is reserved in the host-wide register before anything is awaited,
        // so two creates racing for one name cannot both get it: a minted id is
        // reserved by the draw that found it free, and an explicit one is refused
        // when any root holds it.
        let id = match body.name.clone() {
            Some(id) => {
                if !super::tcp::plain_id(&id) {
                    return Err(format!(
                        "session name {id:?} is not a single plain path segment"
                    ));
                }
                if self.entry(&id).is_some() {
                    return Err(format!("session {id:?} is already hosted"));
                }
                if let Err(held) = self.session_ids.reserve(&id, &project) {
                    return Err(format!(
                        "session name {id:?} is taken by a session under {}",
                        held.root.display()
                    ));
                }
                id
            }
            None => slug::mint(|id| {
                self.entry(id).is_some()
                    || base.join(id).exists()
                    || self.session_ids.reserve(id, &project).is_err()
            }),
        };
        let starting = Starting::new(StartingParts {
            id: id.clone(),
            directories: directories.clone(),
            resume: false,
            created_by: lineage.created_by.clone(),
            detached: body.detached,
        });
        if !self.sessions.lock().unwrap().insert_starting(&starting) {
            self.session_ids.release(&id, &project);
            return Err(format!("session {id:?} is already hosted"));
        }
        self.touch_listing();
        log::info!("session {id}: starting in {}", body.cwd.display());
        let host = Arc::clone(self);
        let preparing = Arc::clone(&starting);
        self.prepare_in_background(&starting, async move {
            host.prepare_creation(&preparing, body, plan, lineage, supplied_env, directories)
                .await
        });
        Ok(starting)
    }

    /// What a creation resolves from its request alone, before anything is reserved:
    /// the role selections and the history and index records a started session
    /// leaves.
    fn plan_creation(
        &self,
        body: &CreateSession,
        project: &std::path::Path,
    ) -> std::result::Result<CreationPlan, String> {
        // The same resolution preview reports, then credentials for the roles it
        // selected. Project effort is boot's to apply, so it is not resolved here.
        // History is resolved here: the omitted branch defaults to past picks
        // in this project, and the write below records an explicit pick.
        let scope = crate::history::Scope::for_request(project, body.config.as_deref());
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
        let triage_explicit = matches!(
            resolved.effective.triage.source,
            Some(crate::inference::SelectionSource::Request)
        );
        // A refusal here is a model or connection the host cannot run, so it names
        // what the host can (docs/model-catalog.md).
        let inference = self
            .inference
            .prepare(resolved)
            .map_err(|error| format!("{error}; {}", self.offered_models()))?;
        // Remember the pick this creation made, for the omitted-branch default
        // and the recent menus. Built before the request moves the inference
        // away; recorded after the session starts, so only sessions that
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
        // The explicit separate triage beside the main model it runs with, kept
        // for later pairing defaults (docs/state-store.md). Never the shared
        // fallback, which has no triage client to name.
        let triage_pick = crate::history::TriagePick::for_creation(
            crate::history::now(),
            &scope,
            triage_explicit,
            inference.main().0,
            inference.triage().map(|(selection, _, _)| selection),
        );
        // Where this session starts, for the index the launch picker reads. Every
        // creation records one — a web launch, a session's own `agents__create`,
        // a `hand_off` in its predecessor's directory — because they are one
        // path here and a launch is what puts a directory on the list. `config` is
        // the template configuration's spelling, never the session's own copy, so
        // the index can hand a client back exactly what to create with.
        let launch = crate::remembered::Project {
            last_used_at: crate::history::now(),
            cwd: scope.cwd.clone(),
            config_key: scope.config_key.clone(),
            config: body.config.clone(),
        };
        Ok(CreationPlan {
            options: body.options.clone(),
            inference,
            pick,
            triage_pick,
            launch,
        })
    }

    /// The rest of a creation, once `starting` holds its id: establish the
    /// environment, prepare, start, and adopt.
    async fn prepare_creation(
        self: &Arc<Self>,
        starting: &Arc<Starting>,
        body: CreateSession,
        plan: CreationPlan,
        lineage: crate::env::Lineage,
        supplied_env: Option<crate::env_table::EnvSnapshot>,
        directories: crate::policy::Directories,
    ) -> std::result::Result<(), String> {
        let id = starting.id().to_string();
        let project = directories.project().to_path_buf();
        // An omitted environment is established here, for the session's project,
        // from the user-wide `env_hook` or the host environment (docs/env.md). The
        // project, not the working directory: resume establishes it there too, before
        // the header says where the session works, so a session gets one environment
        // either way.
        let base_env = match supplied_env {
            Some(base) => base,
            None => self.establish_environment(starting, &project).await?,
        };
        starting.set_stage(Stage::Preparing);
        let CreationPlan {
            options,
            inference,
            pick,
            triage_pick,
            launch,
        } = plan;
        let request = crate::boot::SessionRequest {
            session_dir: project.join(SESSION_BASE).join(&id),
            directories,
            base_env,
            config: body.config,
            inference,
            temperature: body.options.temperature,
            effort: body.options.effort,
            upstream: body.upstream,
            autonomous: body.autonomous,
            reauth: body.reauth,
            authorization: authorization_for(starting, body.refuse_authorization),
            keep_scratch: body.keep_scratch,
            detached: body.detached,
            worktree: body.worktree,
            // The new session's toolbox mounts the `sessions` provider against
            // this host, with the new session itself as the caller (docs/tools.md).
            host: Some(Arc::clone(self)),
            // The creator and predecessor reach the identity line
            // (docs/context.md); adoption below records the same values as
            // control state.
            lineage: lineage.clone(),
        };
        let boot = crate::boot::prepare(request)
            .await
            .map_err(|e| format!("could not prepare session: {e}"))?;
        let runtime = SessionRuntime::start(boot, self.swansong());
        let session = Arc::clone(runtime.session());
        // Adoption replaces this creation's starting entry, and refuses — shutting
        // the runtime down — when anything else holds the id, so nothing here
        // records options or a model pick for a session that will not exist.
        self.adopt_prepared(
            id.clone(),
            runtime,
            lineage.created_by,
            body.detached,
            Some(starting),
        )?;
        self.record_options(&id, options);
        // Remember the pick this creation made, for the omitted-branch default
        // and the recent menus, and the directory it started in, for the index.
        // Both after the session exists, so neither remembers a creation that
        // did not happen; best-effort, so neither store fails one that did.
        if let Some(pick) = pick.as_ref() {
            self.inference.history().record(pick);
        }
        if let Some(pick) = triage_pick.as_ref() {
            self.inference.history().record_triage(pick);
        }
        self.remembered.record(&launch);
        log::info!("session {id}: created in {}", launch.cwd.display());
        // Last, so whoever waits on the start finds everything above in place.
        starting.started(session);
        Ok(())
    }
}

/// What [`Host::plan_creation`] resolved, carried into the background preparation.
struct CreationPlan {
    options: crate::inference::SessionOptions,
    inference: crate::inference::SessionInference,
    pick: Option<crate::history::Pick>,
    triage_pick: Option<crate::history::TriagePick>,
    launch: crate::remembered::Project,
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
    /// The directory the session works in. Absolute, always sent. The session's
    /// project is this directory, or the project whose worktree it is inside
    /// (docs/worktrees.md).
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
    /// Fail the mount of an MCP server that asks for authorization, naming why, instead
    /// of starting the authorization dance on the host (docs/mcp-auth.md): for a
    /// preparation nobody may be at the host to finish.
    #[serde(default)]
    pub refuse_authorization: bool,
    #[serde(default)]
    pub keep_scratch: bool,
    /// Give the session its own git worktree and start it there
    /// (docs/worktrees.md).
    #[serde(default)]
    pub worktree: bool,
    /// Outlive the session that creates this one (docs/tools.md). Meaningful only
    /// for a session-driven `create`: a human root has no creator to end with.
    #[serde(default)]
    pub detached: bool,
    /// The session's complete base environment, when the caller supplies one
    /// (docs/env.md). `None` asks the host to establish it for `cwd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

impl CreateSession {
    /// A creation in `cwd` with every other field at its default: a minted name, the
    /// probed config, the host's defaults for every role, the environment established
    /// by the host.
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            name: None,
            cwd,
            config: None,
            options: SessionOptions::default(),
            upstream: Vec::new(),
            autonomous: false,
            reauth: Vec::new(),
            refuse_authorization: false,
            keep_scratch: false,
            worktree: false,
            detached: false,
            env: None,
        }
    }
}

/// `POST /sessions`: reserve a session from the body and prepare it in the
/// background. Mounted on every host listener, so a network caller reaches this too.
/// `201 {"id"}` once the id is reserved; the preparation shows on the session's own
/// `/state` and `/events` as the `starting` topic (docs/control-api.md).
///
/// Preparation reports to this process's stderr and may open a browser for an MCP
/// authorization, on the host — which a network caller cannot answer, though the
/// `starting` topic names the server it waits on (docs/control-api.md).
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
    match host.create(body, crate::env::Lineage::default()) {
        Ok(starting) => conn
            .with_status(201)
            .with_body(serde_json::json!({ "id": starting.id() }).to_string())
            .halt(),
        Err(reason) => fail(conn, ErrorCode::BadRequest, reason),
    }
}
