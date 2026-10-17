use super::args::{optional_handle, turn_source};
use super::executor::state_of;
use super::{Control, Executor};
use crate::daemon::transcript::Handle;
use crate::daemon::transcript::cursor::ReadPlan;
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Read what an agent said and did: its messages in full, and each call on one line
/// with what became of it. By default, what is new since your last read; `seq`
/// zooms into one entry in full.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "transcript")]
pub struct Transcript {
    /// The agent's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub agent: Option<String>,
    /// One entry in full, by its seq (`8`, or `8.2` for one of several calls).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_handle"
    )]
    #[arg(long, conflicts_with_all = ["since", "tail"])]
    pub seq: Option<String>,
    /// Read from this seq on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(long, conflicts_with = "tail")]
    pub since: Option<u64>,
    /// Read only the last this many entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub tail: Option<u64>,
    /// Read entry by entry, the mechanism included, instead of the conversation; a
    /// window with neither `--since` nor `--tail` is the whole log. Command-line only.
    #[serde(skip)]
    #[schemars(skip)]
    #[arg(long, conflicts_with = "seq")]
    pub audit: bool,
}

impl ToolMeta for Transcript {
    fn title() -> Option<&'static str> {
        Some("Read an agent's transcript")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            // A default read moves the reader's cursor, so the next one differs.
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Transcript {
    /// The zoom's handle, refused beside a window bound.
    fn zoom(&self) -> Result<Option<Handle>> {
        let Some(seq) = &self.seq else {
            return Ok(None);
        };
        if self.since.is_some() || self.tail.is_some() {
            return Err(anyhow!(
                "seq zooms into one entry, so it takes no since or tail; read a window with \
                 since or tail alone"
            ));
        }
        Handle::parse(seq)
            .map(Some)
            .map_err(|reason| anyhow!("{reason}"))
    }
}

impl Tool<Control> for Transcript {
    type Output = String;

    async fn execute(self, control: Control, context: RequestContext) -> Result<String> {
        if self.since.is_some() && self.tail.is_some() {
            return Err(anyhow!(
                "since and tail are mutually exclusive; pass one, or neither for what is new"
            ));
        }
        let zoom = self.zoom()?;
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.agent.as_deref()).await?;
                // The reader, spelled as its turns' `source` is, so its own turns and
                // decisions read "you" (docs/control-api.md).
                let me = turn_source(&context);
                // A zoom sends the handle as given; `zoom` has checked it parses.
                if zoom.is_some() {
                    let read = crate::client::TranscriptRead {
                        seq: self.seq.as_deref(),
                        me: me.as_deref(),
                        ..Default::default()
                    };
                    return client.transcript(read).await;
                }
                if self.audit {
                    let read = crate::client::TranscriptRead {
                        since: self.since,
                        tail: self.tail,
                        audit: true,
                        ..Default::default()
                    };
                    let text = client.transcript(read).await?;
                    return Ok(match text.is_empty() {
                        true => "(no entries)".to_string(),
                        false => text,
                    });
                }
                let state_now = state_of(&client).await?;
                let id = state_now["session_identity"]["id"]
                    .as_str()
                    .map(str::to_string)
                    .or(self.agent.clone())
                    .unwrap_or_default();
                let last = state_now["last_seq"].as_u64();
                let cursor = state.cursor(&id);
                let plan = ReadPlan::new(cursor, self.since, self.tail, last);
                let read = crate::client::TranscriptRead {
                    since: plan.since,
                    before: plan.before,
                    tail: plan.tail,
                    me: me.as_deref(),
                    ..Default::default()
                };
                let text = client.transcript(read).await?;
                if let Some(seq) = plan.cursor_after(cursor) {
                    state.set_cursor(&id, seq);
                }
                Ok(plan.framed(&title(&id), text))
            }
            Executor::Host(state) => {
                let (target, id) = state.resolve(self.agent.as_deref())?;
                let caller = state.caller();
                if let Some(handle) = zoom {
                    return target
                        .transcript_zoom(handle, Some(caller))
                        .map_err(|reason| anyhow!("{reason}"));
                }
                // The host holds the cursor per reader and target (docs/tools.md).
                let host = state.host()?;
                let cursor = host.read_cursor(&id, caller);
                let plan =
                    ReadPlan::new(cursor, self.since, self.tail, target.last_committed_seq());
                let text = target
                    .transcript_window(plan.since, plan.before, plan.tail, caller)
                    .map_err(|reason| anyhow!("{reason}"))?;
                if let Some(seq) = plan.cursor_after(cursor) {
                    host.set_read_cursor(&id, caller, seq);
                }
                Ok(plan.framed(&title(&id), text))
            }
        }
    }
}

/// The header a transcript of `id` opens with.
fn title(id: &str) -> String {
    match id.is_empty() {
        true => "transcript".to_string(),
        false => format!("transcript of {id}"),
    }
}
