use super::{Control, Executor, StateResult};
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Block until the session reaches a decision point — a gate is pending, or the loop
/// has gone idle — and answer with its state then. The wait-without-acting call:
/// nothing is submitted and no gate is resolved, so it is the way to watch where an
/// in-flight turn lands. Returns immediately if the session is already settled.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "settle")]
pub struct Settle {
    /// Seconds to wait before giving up. On expiry the current state comes back
    /// marked `settled: false`, which is not an error. Omitted, the wait has no
    /// bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub timeout_s: Option<u64>,
    /// Watch this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for Settle {
    fn title() -> Option<&'static str> {
        Some("Wait for the session to settle")
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

impl Tool<Control> for Settle {
    type Output = StateResult;

    fn execute(self, control: &mut Control, _context: &RequestContext) -> Result<StateResult> {
        match &mut control.executor {
            Executor::Client(state) => {
                let client = state.client(self.session.as_deref())?;
                state.settle_within(&client, self.timeout_s, client.settle())
            }
            // Hidden from the in-session mount: a session has an inbox to be
            // woken through, not a blocking wait (docs/tools.md).
            Executor::Host(_) => Err(anyhow!(
                "`settle` is not available in-session: a session's continuation is its own inbox, \
                 not a blocking call (docs/tools.md)"
            )),
        }
    }
}
