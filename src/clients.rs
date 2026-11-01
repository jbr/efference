//! The clients of the control channel: the HTTP client every non-loop interaction goes
//! through (`client`), the session-control tools served to agents over it
//! (`control_tools`) and the interactive TUI (`tui`) (docs/README.md, *Dependency direction*).

pub mod client;
pub mod control_tools;
pub mod tui;
