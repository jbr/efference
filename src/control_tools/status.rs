use super::args::reader_name;
use super::executor::{state_of, status_in};
use super::{Control, Executor, HostExecutor};
use crate::daemon::status::{Condition, Moves, Reader, Relation, Reviewer, StatusView, answers};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Where an agent stands and the call that answers it. With no agent, one line for
/// each agent in view.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "status")]
pub struct Status {
    /// The agent's id; omitted, every agent in view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub agent: Option<String>,
}

impl ToolMeta for Status {
    fn title() -> Option<&'static str> {
        Some("Read an agent's status")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

/// How an operator client stands to a session (docs/tools.md): it reviews a gate
/// routed to the operator, and drives everything else.
pub(super) fn operator_relation(view: &StatusView) -> Relation {
    match &view.condition {
        Condition::Gated(held) if held.reviewer == Reviewer::Operator => Relation::Reviewer,
        _ => Relation::Driver,
    }
}

/// How the calling session stands to a session (docs/tools.md): it reviews a gate
/// routed to it, drives what it drives, and observes the rest.
pub(super) fn session_relation(state: &HostExecutor, view: &StatusView, drives: bool) -> Relation {
    match &view.condition {
        Condition::Gated(held)
            if held.reviewer
                == (Reviewer::Supervisor {
                    id: state.caller().to_string(),
                }) =>
        {
            Relation::Reviewer
        }
        _ if drives => Relation::Driver,
        _ => Relation::Observer,
    }
}

impl Tool<Control> for Status {
    type Output = String;

    async fn execute(self, control: Control, context: RequestContext) -> Result<String> {
        let now = super::now();
        match &control.executor {
            Executor::Client(state) => {
                let me = reader_name(&context);
                if self.agent.is_none() && state.host_wide() {
                    let host = state.host_client()?;
                    let mut lines = Vec::new();
                    for row in host.sessions().await? {
                        let client = host.clone().with_prefix(&format!("/sessions/{}", row.id));
                        let Ok(view) = status_in(&state_of(&client).await?) else {
                            continue;
                        };
                        let reader =
                            Reader::read(operator_relation(&view), &Moves::MCP, me.as_deref());
                        lines.push(answers::overview_line(&view, &reader, false, now));
                    }
                    return Ok(answers::overview(lines, "the host holds no agents"));
                }
                let client = state.client(self.agent.as_deref()).await?;
                let view = status_in(&state_of(&client).await?)?;
                let reader = Reader::read(operator_relation(&view), &Moves::MCP, me.as_deref());
                Ok(crate::daemon::status::render(&view, &reader, now))
            }
            Executor::Host(state) => {
                let host = state.host()?;
                let caller = state.caller();
                let Some(agent) = self.agent.as_deref() else {
                    let mut lines = Vec::new();
                    for id in state.project_ids()? {
                        let Some(view) = host.status_view_of(&id) else {
                            continue;
                        };
                        let drives = state.drives(&id)?;
                        let relation = session_relation(state, &view, drives);
                        let reader = Reader::read(relation, &Moves::IN_SESSION, Some(caller));
                        lines.push(answers::overview_line(&view, &reader, drives, now));
                    }
                    return Ok(answers::overview(
                        lines,
                        "no other agents in this project; `create` starts one",
                    ));
                };
                let (target, id) = state.resolve(Some(agent))?;
                let view = target.status_view(&id, &target.status());
                let relation = session_relation(state, &view, state.drives(&id)?);
                let reader = Reader::read(relation, &Moves::IN_SESSION, Some(caller));
                Ok(crate::daemon::status::render(&view, &reader, now))
            }
        }
    }
}
