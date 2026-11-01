//! The control channel — the daemon and its HTTP-over-Unix-socket interface
//! (docs/control-channel.md).
//!
//! The HTTP interface is the singular input source: the loop's only [`Frontend`]
//! is [`RemoteFrontend`], fed by the handlers here, so there is no privileged
//! in-process path. Human (TUI), driver, and subagent are all clients of the same
//! socket. A session's own state is [`Session`](crate::session::Session).

use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_lite::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, broadcast, mpsc};
use trillium::Conn;
use trillium_router::{Router, RouterConnExt};
use trillium_sse::{Eventable, sse};

use crate::agent;
use crate::inference::api::Error;

use crate::config::LoadedConfig;
use crate::curation::spotter::Spotter;
use crate::frontend::Frontend;
use crate::protocol::state::{LoopPhase, SessionIdentity, Topic};
use crate::protocol::stream::StreamDelta;
use crate::protocol::turns::mint_submission_id;
use crate::record::log::{
    Approval, CallId, EditState, LogEntry, Notification, Payload, RawResult, Seq, ToolCall,
};
use crate::record::persist::Log;
use crate::session::approvals::GateOptions;
use crate::session::submitted::SubmittedTurn;
use crate::tools::{Interrupt, Toolbox};

mod entry_view;
// The typed error envelope (docs/control-api.md) — every non-2xx body the router
// answers with.
pub(crate) mod errors;
// The file viewer's read surface (docs/web-bind.md), mounted on the TCP spawn only.
pub(crate) mod files;
// Fenced HTML artifacts' signed links and routes (docs/artifacts.md).
pub mod artifacts;
// The control-stream frame envelope (docs/control-api.md). `dead_code` covers
// `Frame::with_render` and `Frame::state`, which the emitter does not reach:
// nothing annotates a frame with a render yet, and the state path builds from
// pre-serialized topic JSON (`Frame::state_raw`).
#[allow(dead_code)]
mod frames;
mod handler;
mod host_http;
mod host_routes;
mod http;
mod stream;
pub(crate) mod tcp;
mod views;

/// The one handler construction (docs/web-bind.md): every listener mounts
/// [`handler()`], keyed on its [`Transport`] and [`App`]. Public so a client of the
/// control channel can mount the socket-shaped stack on an in-memory transport and
/// exercise a real request/response cycle without binding anything.
pub use handler::{App, Transport, handler};

use entry_view::{EntryView, result_raw};
use errors::{ErrorBody, ErrorCode, fail, respond};
use frames::Frame;
use http::LogSelect;
use stream::{Include, control_stream};

#[cfg(test)]
mod test;

mod frontend;
mod host;
pub mod island;
pub(crate) mod permissions;
mod runtime;
mod serve;
pub mod slug;
mod starting;
mod subscriptions;
pub use host::Host;
pub use runtime::{SessionBoot, SessionRuntime};
pub use serve::serve_host;

pub use frontend::RemoteFrontend;
pub use tcp::{TcpBind, TlsPem};
