//! Minting a session (docs/control-api.md): the host's side of `POST /sessions`, whose
//! route is `host_http/create.rs`.
//!
//! The counterpart of [`resume`]: the caller supplies what a log cannot hold, the
//! host prepares a session and starts it, and the id comes back.

use super::preparation::authorization_for;
use super::*;
use crate::host::starting::{Starting, StartingParts};
use crate::protocol::state::Stage;
use crate::record::session_dir::{SESSION_BASE, plain_id};

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
    /// [`Starting::finished`](crate::host::starting::Starting::finished).
    pub(crate) fn create(
        self: &Arc<Self>,
        mut body: CreateSession,
        lineage: crate::config::env::Lineage,
    ) -> Result<Arc<Starting>, String> {
        body.cwd = crate::platform::home::expand_here(&body.cwd);
        if !body.cwd.is_absolute() {
            return Err(format!(
                "cwd must be absolute or start with ~: {}",
                body.cwd.display()
            ));
        }
        // The session is rooted at its project and works in the directory the client
        // sent: they differ when that directory is inside a worktree
        // (docs/worktrees.md).
        let project = crate::record::session_dir::worktree::project_of(&body.cwd)
            .unwrap_or_else(|| body.cwd.clone());
        let directories =
            crate::config::policy::Directories::new(project.clone(), body.cwd.clone());
        // An environment the caller supplied is the base, and nothing is run.
        let supplied_env = match body.env.take() {
            Some(env) => Some(
                crate::config::env_table::EnvSnapshot::new(env).map_err(|e| format!("env: {e}"))?,
            ),
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
                if !plain_id(&id) {
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
    ) -> Result<CreationPlan, String> {
        // The same resolution preview reports, then credentials for the roles it
        // selected. Project effort is boot's to apply, so it is not resolved here.
        // History is resolved here: the omitted branch defaults to past picks
        // in this project, and the write below records an explicit pick.
        let scope = crate::config::history::Scope::for_request(project, body.config.as_deref());
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
            crate::config::history::Pick {
                at: crate::config::history::now(),
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
        let triage_pick = crate::config::history::TriagePick::for_creation(
            crate::config::history::now(),
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
        let launch = remembered::Project {
            last_used_at: crate::config::history::now(),
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
        lineage: crate::config::env::Lineage,
        supplied_env: Option<crate::config::env_table::EnvSnapshot>,
        directories: crate::config::policy::Directories,
    ) -> Result<(), String> {
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
        let detached = body.detached;
        let request = boot::SessionRequest {
            session_dir: project.join(SESSION_BASE).join(&id),
            directories,
            base_env,
            inference,
            authorization: authorization_for(starting, body.refuse_authorization),
            // The request travels whole: boot reads the options it uses from the body.
            create: body,
            // The new session's toolbox mounts the `sessions` provider against
            // this host, with the new session itself as the caller (docs/tools.md).
            host: Some(Arc::clone(self)),
            // The creator and predecessor reach the identity line
            // (docs/context.md); adoption below records the same values as
            // control state.
            lineage: lineage.clone(),
        };
        let boot = boot::prepare(request)
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
            detached,
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
    options: SessionOptions,
    inference: crate::inference::SessionInference,
    pick: Option<crate::config::history::Pick>,
    triage_pick: Option<crate::config::history::TriagePick>,
    launch: remembered::Project,
}
