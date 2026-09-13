use super::{Control, Executor};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Set the operator's pace dial: how long the session waits between turns, and
/// whether it is held at the next between-turns juncture. Transparent to the agent —
/// the pace is never logged and never sent to the model. `hold` pauses without
/// losing the speed underneath it; `resume` releases at that retained speed.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "pace")]
pub struct Pace {
    /// Between-turns delay in milliseconds. 0 is as fast as possible.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub speed_ms: Option<u64>,
    /// Pause at the next between-turns juncture.
    #[serde(default)]
    #[arg(long, conflicts_with = "resume")]
    pub hold: bool,
    /// Release a hold.
    #[serde(default)]
    #[arg(long)]
    pub resume: bool,
    /// Act on this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for Pace {
    fn title() -> Option<&'static str> {
        Some("Set the pace dial")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for Pace {
    type Output = String;

    fn execute(self, control: &mut Control, _context: &RequestContext) -> Result<String> {
        let Executor::Client(state) = &mut control.executor;
        let client = state.client(self.session.as_deref())?;
        // `hold`/`resume` move the held overlay; absent both, only the speed dial (if
        // given) moves. The clap path already rejects `--hold --resume`.
        let held = match (self.hold, self.resume) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        };
        let (speed, held) = state.block_on(client.pace(self.speed_ms, held))?;
        Ok(format!(
            "pace: speed {speed}ms, {}",
            if held { "HELD" } else { "running" }
        ))
    }
}
