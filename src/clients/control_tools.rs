//! The session-control tools: each verb declared once, for every set that lists it
//! (docs/tools.md, docs/operating.md).
//!
//! A verb is a tool struct in a file of its own — its arguments, its title and hints, and
//! a [`ControlVerb`](verb::ControlVerb) impl saying what it does through each executor —
//! and a row of the verb table below, which names it and marks the sets that list it:
//!
//! - [`in_session`], mounted in a hosted session's toolbox under `agents__<verb>`
//!   (`Provider::Sessions`);
//! - [`mcp`], served over stdin/stdout by `efference mcp`;
//! - [`cli`], the clap `Subcommand` behind `efference control <verb>`: the agent verbs
//!   plus the operator's route verbs.
//!
//! From the table, `verbs!` (`verb.rs`) writes each struct's mcplease `Tool` impl and
//! the three sets' `tools!` enums. A set is an allow-list: the stdio mount serves its
//! whole enum, and mcplease has no way to hide a tool from it. The `agent` argument the
//! verbs share is declared once for each form it takes (`args.rs`).
//!
//! One module name dodges a dependency of the same name (`use image::Image` would be
//! ambiguous against the `image` crate): the `image` tool lives in `read_image.rs`.
//! The advertised tool name is unaffected.
//!
//! `Tool::execute` is async, so each tool awaits its calls on whatever runtime
//! drives it: the binary's for the command line and `efference mcp`, the host's
//! for a session's own toolbox.

use std::path::PathBuf;

use crate::protocol::agents::BoundAction;
use crate::record::log::ToolCall;
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
mod instructions;
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
mod verb;
mod wait;
mod watch;

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
pub use instructions::MCP_INSTRUCTIONS;
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

// The verb table (docs/tools.md): each verb's tool struct, the name it is advertised
// under, and the sets that list it — a session's own toolbox, `efference mcp`, and
// `efference control` — with `-` for a set that does not. A set lists its verbs in table
// order, which is the order a mounting harness caches and a session's `tools[]` carries.
// `status` has a struct per set, since the MCP one adds `follow` and the command line's
// repeats `--agent` and prints JSON on request; `as Status` names the latter two as the
// command line names its subcommand.
verb::verbs! {
    //  struct                  name             in a session  efference mcp  efference control
    Create                      "create"         [session      mcp            cli];
    Message                     "message"        [session      mcp            cli];
    Decide                      "decide"         [session      mcp            cli];
    Interrupt                   "interrupt"      [session      mcp            cli];
    Archive                     "archive"        [session      mcp            cli];
    Status                      "status"         [session      -              -  ];
    McpStatus as Status         "status"         [-            mcp            -  ];
    CliStatus as Status         "status"         [-            -              cli];
    Transcript                  "transcript"     [session      mcp            cli];
    HandOff                     "hand_off"       [session      -              -  ];
    Wait                        "wait"           [-            mcp            -  ];
    Settle                      "settle"         [-            -              cli];
    State                       "state"          [-            -              cli];
    Pace                        "pace"           [-            -              cli];
    Title                       "title"          [-            -              cli];
    Background                  "background"     [-            -              cli];
    Proposal                    "proposal"       [-            -              cli];
    ReviewLevel                 "review_level"   [-            -              cli];
    Supervisor                  "supervisor"     [-            -              cli];
    Notify                      "notify"         [-            -              cli];
    Entries                     "entries"        [-            -              cli];
    Image                       "image"          [-            -              cli];
}

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

    /// The state a hosted session's own toolbox mounts (docs/tools.md): the session
    /// `caller` driving its host's sessions through the in-process executor.
    pub fn in_session(host: &std::sync::Arc<crate::host::Host>, caller: String) -> Self {
        Self::new(Executor::host(host, caller), false)
    }
}

/// The in-session set's server for the session `caller` on `host`, ready for its toolbox
/// to mount (docs/tools.md): what the process serving a host installs with
/// `Host::set_session_control`, since the host does not name these verbs.
pub fn in_session_server(
    host: &std::sync::Arc<crate::host::Host>,
    caller: String,
) -> Box<dyn crate::tools::ControlServer> {
    crate::tools::control_server(Control::in_session(host, caller))
}

/// The in-session set, mounted in a session's toolbox as `Provider::Sessions`
/// (docs/tools.md).
impl crate::tools::SessionControl for Control {
    type Verbs = in_session::Tools;

    /// Whether `call` is this session archiving its own predecessor — the session whose
    /// recorded succession names the caller (docs/permissions.md). Only the in-process
    /// [`Executor::Host`] knows the caller; the host wrote the link at `hand_off`, so the
    /// agent cannot assert it.
    fn completes_hand_off(&self, call: &ToolCall) -> bool {
        match &self.executor {
            Executor::Host(state) => state.completes_hand_off(call),
            Executor::Client(_) => false,
        }
    }

    /// Resolve an allowing `agents__decide` call to the leaf action it authorizes
    /// (docs/permissions.md). Only the in-process [`Executor::Host`] can: it knows
    /// the caller and can read the target's pending gate. The HTTP executor never
    /// sees this call — the binding happens inside the target's own daemon.
    fn resolve_bound_action(&self, call: &ToolCall) -> Result<BoundAction> {
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
    pub fn host(host: &std::sync::Arc<crate::host::Host>, caller: String) -> Self {
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
    crate::record::log::Timestamp::now().0
}

#[cfg(test)]
mod test;
