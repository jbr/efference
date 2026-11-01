//! The listeners serving a host over its control socket and optional TCP bind
//! (docs/README.md, *Dependency direction*).

// Fenced HTML artifacts' signed links and routes (docs/artifacts.md).
pub mod artifacts;
// The file viewer's read surface (docs/web-bind.md), mounted on the TCP spawn only.
pub(crate) mod files;
// The one handler construction (docs/web-bind.md): every listener mounts
// [`handler()`](handler::handler), keyed on its [`Transport`](handler::Transport) and
// [`App`](handler::App). Public so a client of the control channel can mount the
// socket-shaped stack on an in-memory transport and exercise a real request/response
// cycle without binding anything.
pub mod handler;
pub(crate) mod host_routes;
pub mod serve;
pub mod tcp;
