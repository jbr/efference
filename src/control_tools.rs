//! The session-control tools: one definition per verb, three tool sets over them
//! (docs/tools.md, docs/operating.md).
//!
//! Each verb is written once here as an [mcplease] tool struct, and three
//! `tools!` invocations list them:
//!
//! - [`in_session`], mounted in a hosted session's toolbox under `agents__<verb>`
//!   (`Provider::Sessions`);
//! - [`mcp`], served over stdin/stdout by `efference mcp`;
//! - [`cli`], the clap `Subcommand` behind `efference control <verb>`: the agent verbs
//!   plus the operator's route verbs.
//!
//! Each set names the shared structs in scope (`tools!(…, modules = false, …)`), since
//! mcplease's default form declares a module per tool and a module has one parent. A
//! set is an allow-list: the stdio mount serves its whole enum, and mcplease has no
//! way to hide a tool from it.
//!
//! One module name dodges a dependency of the same name (`use image::Image` would be
//! ambiguous against the `image` crate): the `image` tool lives in `read_image.rs`.
//! The advertised tool name is unaffected.
//!
//! `Tool::execute` is async, so each tool awaits its calls on whatever runtime
//! drives it: the binary's for the command line and `efference mcp`, the host's
//! for a session's own toolbox.

use std::path::PathBuf;

use crate::log::ToolCall;
use crate::protocol::agents::BoundAction;
use anyhow::{Result, anyhow};

mod archive;
mod args;
mod background;
mod cli_status;
mod create;
mod decide;
mod entries;
mod executor;
mod follow;
mod hand_off;
mod host_executor;
mod interrupt;
mod message;
mod notify;
mod pace;
mod proposal;
mod read_image;
mod results;
mod review_level;
mod settle;
mod state;
mod status;
mod stream;
mod supervisor;
mod title;
mod transcript;
mod wait;
mod watch;

pub mod cli;
pub mod in_session;
pub mod mcp;

pub use archive::Archive;
pub use background::Background;
pub use cli_status::{CliStatus, Output};
pub use create::Create;
pub use decide::Decide;
pub use entries::Entries;
#[cfg(test)]
pub(crate) use executor::named;
pub use executor::{ClientExecutor, Fallback};
#[cfg(test)]
pub(crate) use follow::follow_status;
pub use hand_off::HandOff;
pub use host_executor::HostExecutor;
pub use interrupt::Interrupt;
pub use message::Message;
pub use notify::Notify;
pub use pace::Pace;
pub use proposal::Proposal;
pub use read_image::Image;
pub use results::{EntriesResult, ImageResult, StateResult};
pub use review_level::ReviewLevel;
pub use settle::Settle;
pub use state::State;
pub use status::{McpStatus, Status};
pub use supervisor::Supervisor;
pub use title::Title;
pub use transcript::Transcript;
pub use wait::Wait;

/// The tool state: an executor and the presentation requested by its mount.
/// Cloned into every call, which may run alongside others.
#[derive(Clone)]
pub struct Control {
    executor: Executor,
    /// Whether this dispatch came from the command line rather than from an MCP
    /// client. `image` reads it — at a terminal it writes a file and says so, over MCP
    /// it hands back the bytes as an image content block — and `create`, whose
    /// creation an MCP mount watches for `wait` (docs/operating.md).
    cli: bool,
}

impl Control {
    /// Bind tools to an executor. `cli` selects the command line's presentation.
    pub fn new(executor: Executor, cli: bool) -> Self {
        Self { executor, cli }
    }

    /// Whether this dispatch came from the command line. See [`Control::cli`].
    fn is_cli(&self) -> bool {
        self.cli
    }

    /// Whether `call` is this session archiving its own predecessor — the session whose
    /// recorded succession names the caller (docs/permissions.md). Only the in-process
    /// [`Executor::Host`] knows the caller; the host wrote the link at `hand_off`, so the
    /// agent cannot assert it.
    pub(crate) fn completes_hand_off(&self, call: &ToolCall) -> bool {
        match &self.executor {
            Executor::Host(state) => state.completes_hand_off(call),
            Executor::Client(_) => false,
        }
    }

    /// Resolve an allowing `agents__decide` call to the leaf action it authorizes
    /// (docs/permissions.md). Only the in-process [`Executor::Host`] can: it knows
    /// the caller and can read the target's pending gate. The HTTP executor never
    /// sees this call — the binding happens inside the target's own daemon.
    pub(crate) fn resolve_bound_action(&self, call: &ToolCall) -> Result<BoundAction> {
        match &self.executor {
            Executor::Host(state) => state.resolve_bound_action(call),
            Executor::Client(_) => Err(anyhow!(
                // docs/tools.md
                "no in-process host to resolve a bound action against"
            )),
        }
    }
}

/// How control tools reach a session (docs/tools.md). The CLI and stdio MCP
/// server use the HTTP client; a session's own toolbox uses the in-process
/// `Host` executor and calls the host directly. Each tool explicitly selects
/// its executor in its `execute` implementation.
#[derive(Clone)]
pub enum Executor {
    Client(ClientExecutor),
    Host(HostExecutor),
}

impl Executor {
    /// Bind the HTTP executor to a host socket and fallback session selection.
    pub fn client(host: PathBuf, fallback: Fallback) -> Self {
        Self::Client(ClientExecutor::new(host, fallback))
    }

    /// Bind the in-process executor: the calling session driving its project's
    /// sessions through the host it already lives in (docs/tools.md). The host
    /// is held weakly — the host owns every session and a session's toolbox
    /// holds this executor, so a strong reference would be a cycle.
    pub fn host(host: &std::sync::Arc<crate::daemon::Host>, caller: String) -> Self {
        Self::Host(HostExecutor::new(host, caller))
    }
}

/// What a verb that is not on the agent surfaces answers when an in-session executor
/// reaches it: these are the operator's, on `efference control` (docs/operating.md).
fn operator_verb(name: &str) -> anyhow::Error {
    // docs/tools.md
    anyhow!("`{name}` is an operator verb on `efference control`, not mounted in a session")
}

/// The clock the answers measure durations against, in Unix millis.
fn now() -> u64 {
    crate::log::Timestamp::now().0
}

#[cfg(test)]
mod test;
