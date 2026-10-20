use super::args::{reader_name, turn_source};
use super::executor::status_in;
use super::status::operator_relation;
use super::{Control, Executor};
use crate::daemon::status::{Condition, Moves, Reader, Relation, answers};
use anyhow::{Context, Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Start an agent: a new session on this host, optionally with its first message.
/// Answers with its id, model, working directory, where its gates go, and its
/// status. The model is `<connection>/<model>`; omitted, it takes past picks.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "create")]
pub struct Create {
    /// The working directory, relative to yours; defaults to yours.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// The id, a single path segment. Minted (`adjective-noun`) when omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub name: Option<String>,
    /// Its first message.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub message: Option<String>,
    /// The main model, `<connection>/<model>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub model: Option<String>,
    /// The spotter model, `<connection>/<model>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub spotter_model: Option<String>,
    /// A config file to seed the session's `config.toml` from, instead of the project template.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Reasoning effort for the main model, as the provider names its levels.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub effort: Option<String>,
    /// Sampling temperature, 0–100 (percent). 0 is (near-)deterministic; omitted
    /// uses the provider default (docs/transport.md).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    #[schemars(range(min = 0, max = 100))]
    pub temperature: Option<u8>,
    /// Run without anyone at the gate: calls the permission rungs do not clear are declined rather than held.
    #[serde(default)]
    #[arg(long)]
    pub autonomous: bool,
    /// Pin an OpenRouter session to these endpoints, in order (`google-ai-studio`, `google-vertex/global`); the request fails rather than falling back past them. Repeatable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[arg(long)]
    pub upstream: Vec<String>,
    /// Let it outlive you. By default an agent ends when the session that created it ends.
    #[serde(default)]
    #[arg(long)]
    pub detached: bool,
    /// Give it its own checkout and branch to commit on, so its edits do not land in your tree. Without it, it works where you work (in your worktree, if you have one).
    #[serde(default)]
    #[arg(long)]
    pub worktree: bool,
}

impl ToolMeta for Create {
    fn title() -> Option<&'static str> {
        Some("Create an agent")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            // Two identical creates are two sessions.
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Create {
    /// The creation request, rooted at `here`, with `env` as the new session's base.
    fn request(
        &self,
        here: PathBuf,
        env: std::collections::BTreeMap<String, String>,
    ) -> crate::daemon::CreateSession {
        let cwd = crate::policy::lexical_normalize(&match &self.cwd {
            Some(cwd) => here.join(cwd),
            None => here,
        });
        crate::daemon::CreateSession {
            name: self.name.clone(),
            cwd,
            config: self.config.clone(),
            // The agent-facing surface takes one opaque model slug; `connection`
            // stays absent so it resolves as one (docs/control-api.md).
            options: crate::inference::SessionOptions {
                model: self.model.clone(),
                spotter_model: self.spotter_model.clone(),
                temperature: self.temperature,
                effort: self.effort.clone(),
                ..Default::default()
            },
            upstream: self.upstream.clone(),
            autonomous: self.autonomous,
            reauth: Vec::new(),
            refuse_authorization: false,
            keep_scratch: false,
            worktree: self.worktree,
            detached: self.detached,
            env: Some(env),
        }
    }

    /// The first message, when it says anything.
    fn first_message(&self) -> Option<&str> {
        self.message
            .as_deref()
            .filter(|text| !text.trim().is_empty())
    }
}

/// The created session's model, `<connection>/<model>`, from its identity.
fn model_of(identity: &serde_json::Value) -> Option<String> {
    let main = &identity["inference"]["main"];
    match (main["connection"].as_str(), main["model"].as_str()) {
        (Some(connection), Some(model)) => Some(format!("{connection}/{model}")),
        _ => identity["model"].as_str().map(str::to_string),
    }
}

impl Tool<Control> for Create {
    type Output = String;

    async fn execute(self, control: Control, context: RequestContext) -> Result<String> {
        let now = super::now();
        match &control.executor {
            Executor::Client(state) => {
                let here = std::env::current_dir().context("reading the working directory")?;
                let env = crate::env_table::EnvSnapshot::capture()
                    .map_err(anyhow::Error::msg)?
                    .into_entries();
                let request = self.request(here, env);
                let host = state.host_client()?;
                let id = host.create_session(&request).await?;
                // Wait out the preparation: a starting session's wait settles when it
                // has started and settled, or failed (docs/control-api.md).
                let client = host.clone().with_prefix(&format!("/sessions/{id}"));
                let started = state.settle_within(&client, None, client.settle()).await?;
                let state_now: serde_json::Value = serde_json::from_str(&started.body)?;
                let view = status_in(&state_now)?;
                if let Condition::FailedToStart { reason } = &view.condition {
                    return Err(anyhow!("{id} did not start: {reason}"));
                }
                // An MCP mount watches what it creates, from the status this answer
                // shows, so `wait` reports what comes after (docs/operating.md). The
                // command line holds no mount to watch from.
                let watched = !control.is_cli();
                if watched {
                    state.watch(&id, Some(&view))?;
                }
                let me = reader_name(&context);
                // Its status stream, for a client that holds one open (docs/operating.md).
                // The agent exists either way, so a server that cannot bind is a line in
                // the answer rather than an error.
                let follow = match watched {
                    true => Some(match state.stream_url(&id, me.as_deref()).await {
                        Ok(url) => answers::follow(&url),
                        Err(e) => answers::no_stream(&e),
                    }),
                    false => None,
                };
                let reader = Reader::read(operator_relation(&view), &Moves::MCP, me.as_deref());
                let then = match self.first_message() {
                    Some(text) => {
                        let source = turn_source(&context);
                        let turn = crate::frontend::SubmittedTurn::new(
                            text,
                            crate::daemon::mint_submission_id(),
                        )
                        .sourced(source);
                        match client.submit(&turn, None).await? {
                            Ok(landing) => answers::sent(&id, landing, Some(&view), now),
                            Err(refusal) => format!("first message not delivered: {refusal}"),
                        }
                    }
                    None => answers::fresh(&view, &reader, now),
                };
                let row = host.sessions().await?.into_iter().find(|row| row.id == id);
                let identity = &state_now["session_identity"];
                let working = row
                    .as_ref()
                    .and_then(|row| row.working.clone().or(row.cwd.clone()))
                    .or_else(|| identity["cwd"].as_str().map(str::to_string))
                    .unwrap_or_default();
                let project = row
                    .as_ref()
                    .filter(|row| row.working.is_some())
                    .and_then(|row| row.cwd.clone());
                Ok(answers::created(&answers::Created {
                    id: &id,
                    model: model_of(identity),
                    working: &working,
                    project: project.as_deref(),
                    branch: self.worktree.then(|| crate::worktree::branch(&id)),
                    gates: answers::gates("the operator", self.autonomous),
                    watched,
                    follow,
                    then,
                }))
            }
            Executor::Host(state) => {
                // A session-driven creation runs in the caller's cwd and with the
                // caller's child environment as the new session's base — the same
                // environment its own bash children get (docs/tools.md).
                let caller = state.caller_session()?;
                let request =
                    self.request(caller.cwd().to_path_buf(), caller.child_env().resolved());
                // Reach is the project (docs/tools.md): every later verb finds a
                // target only under the caller's `.efference/` base, so a child
                // minted anywhere else could never be supervised or ended by the
                // session that commissioned it. Refused here, before anything is
                // minted, rather than discovered at the first `status` call. The
                // child's project is resolved as the host will resolve it: a `cwd`
                // inside a worktree belongs to that worktree's project
                // (docs/worktrees.md).
                let cwd = request.cwd.clone();
                let child_project =
                    crate::worktree::project_of(&cwd).unwrap_or_else(|| cwd.clone());
                let base = child_project.join(crate::daemon::SESSION_BASE);
                let project = caller
                    .storage_base()
                    .map(crate::policy::lexical_normalize)
                    .ok_or_else(|| anyhow!("the calling session has no project base"))?;
                if base != project {
                    anyhow::bail!(
                        "refused: {} is outside this project — a session creates only under \
                         its own `.efference/` base ({}) so it can reach what it made; \
                         create under this project's directory instead",
                        cwd.display(),
                        project.display()
                    );
                }
                // The child records the caller as its creator, which is what makes
                // it a descendant for later driving verbs — and subscribes the caller
                // to the child's status (docs/tools.md).
                let (child, id) = state.create(request).await?;
                let view = child.status_view(&id, &child.status());
                let reader =
                    Reader::read(Relation::Driver, &Moves::IN_SESSION, Some(state.caller()));
                let then = match self.first_message() {
                    Some(text) => {
                        let turn = crate::frontend::SubmittedTurn::new(
                            text,
                            crate::daemon::mint_submission_id(),
                        )
                        .sourced(Some(crate::frontend::session_source(state.caller())));
                        match child.submit_named_turn(turn, None) {
                            Ok(landing) => answers::sent(&id, landing, Some(&view), now),
                            Err(refusal) => format!(
                                "first message not delivered: {}",
                                refusal.message(&id, Some(state.caller()))
                            ),
                        }
                    }
                    None => answers::fresh(&view, &reader, now),
                };
                let directories = child.directories();
                let working = directories.working().display().to_string();
                let project = (directories.working() != directories.project())
                    .then(|| directories.project().display().to_string());
                let identity = child
                    .identity()
                    .and_then(|identity| serde_json::to_value(identity).ok())
                    .unwrap_or_default();
                Ok(answers::created(&answers::Created {
                    id: &id,
                    model: model_of(&identity),
                    working: &working,
                    project: project.as_deref(),
                    branch: self.worktree.then(|| crate::worktree::branch(&id)),
                    gates: answers::gates("you", self.autonomous),
                    watched: false,
                    follow: None,
                    then,
                }))
            }
        }
    }
}
