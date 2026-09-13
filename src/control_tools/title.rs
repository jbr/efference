use super::{Control, Executor};
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Set the session's title: the one line the session list, the TUI sidebar, and
/// the web top bar show for it. The triage model keeps a title current on its
/// own; this overrides it, and the next model-written title starts from what you
/// set. The session's id (its address) never changes.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "title")]
pub struct Title {
    /// The title, one line.
    pub title: String,
    /// Act on this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for Title {
    fn title() -> Option<&'static str> {
        Some("Set the session title")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for Title {
    type Output = String;

    fn execute(self, control: &mut Control, _context: &RequestContext) -> Result<String> {
        match &mut control.executor {
            Executor::Client(state) => {
                let client = state.client(self.session.as_deref())?;
                let identity = state.block_on(client.set_title(&self.title))?;
                Ok(format!(
                    "{}: {}",
                    identity["id"].as_str().unwrap_or("session"),
                    identity["title"].as_str().unwrap_or_default()
                ))
            }
            Executor::Host(state) => {
                let session = state.resolve_mutating(self.session.as_deref())?;
                // The same shape the `PUT /title` route enforces: whitespace-trimmed,
                // non-empty, within the peek's length cap.
                let text = self.title.trim();
                if text.is_empty() {
                    return Err(anyhow!("title must not be empty"));
                }
                if text.chars().count() > crate::distill::TITLE_MAX_CHARS {
                    return Err(anyhow!(
                        "title must be at most {} characters",
                        crate::distill::TITLE_MAX_CHARS
                    ));
                }
                let title = crate::log::Title {
                    text: text.to_string(),
                    author: crate::log::TitleAuthor::Operator,
                };
                match session.set_title(title) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return Err(anyhow!(
                            "this session has no identity to title (no daemon booted it)"
                        ));
                    }
                    Err(e) => return Err(anyhow!("could not record the title: {e}")),
                }
                let identity = session.identity();
                let id = identity
                    .as_ref()
                    .map(|i| i.id.as_str())
                    .unwrap_or("session");
                let title = identity
                    .as_ref()
                    .and_then(|i| i.title.as_deref())
                    .unwrap_or_default();
                Ok(format!("{id}: {title}"))
            }
        }
    }
}
