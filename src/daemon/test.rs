use super::http::DecisionBody;
use super::*;
use crate::client::Client as ControlClient;
use crate::log::ContentBlock;
use futures_lite::StreamExt;
use std::time::Duration;

fn user(text: &str) -> Payload {
    Payload::User(vec![ContentBlock::Text(text.into())])
}

/// One bash `ToolCall`, for exercising the effective-policy cache through `classify`.
fn bash_call(command: &str) -> ToolCall {
    ToolCall {
        id: CallId("toolu_x".into()),
        name: crate::bash::NAME.into(),
        input: serde_json::json!({ "command": command }),
    }
}

/// A role that resolved from `path`, so `Shared` opens it as the live [`ConfigFile`] — what
/// makes the refresh/revert path reachable in a test without a real daemon boot. The base role
/// stays default: the file's contents reach effective policy through a refresh, like any edit.
fn file_role(path: &std::path::Path) -> LoadedConfig {
    LoadedConfig {
        role: crate::config::Config::default(),
        source: ConfigSource::Path(path.to_path_buf()),
    }
}

fn allow_grant(program: &str) -> Payload {
    committed_edit(crate::log::ConfigChange::Command {
        command: program.into(),
    })
}

/// A committed config edit (the synchronous/direct commit path) as a `Payload` — no prior
/// proposal to resolve.
fn committed_edit(change: crate::log::ConfigChange) -> Payload {
    Payload::ConfigEdit(crate::log::ConfigEdit {
        state: EditState::Committed,
        resolves: None,
        change,
        note: String::new(),
    })
}

/// A `Proposed` edit as a `Payload`, for driving the lifecycle in tests.
fn proposed_edit(change: crate::log::ConfigChange) -> Payload {
    Payload::ConfigEdit(crate::log::ConfigEdit {
        state: EditState::Proposed,
        resolves: None,
        change,
        note: String::new(),
    })
}

/// (id, event_type, data) of the next SSE event, with a timeout so a hung tail
/// fails loudly instead of blocking the suite.
async fn next_event(
    events: &mut trillium_client::sse::EventStream,
) -> (Option<String>, Option<String>, String) {
    let event = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("an event within the timeout")
        .expect("the stream has not ended")
        .expect("the event parsed");
    (
        event.id().map(str::to_string),
        event.event_type().map(str::to_string),
        event.data().to_string(),
    )
}

mod approvals;
mod background;
mod config_edits;
mod http;
mod metrics;
mod notifications;
mod pace;
mod sse;
mod views;
