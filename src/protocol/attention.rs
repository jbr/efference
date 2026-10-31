//! A client's report of whether anyone is attending (docs/web-push.md): the body of
//! `POST /attention`, from which the host judges whether a push would reach someone
//! who is already looking.

use serde::{Deserialize, Serialize};

/// One client's report, as `POST /attention` receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionReport {
    /// A per-page-load (web) or per-process (TUI) id the client generated.
    pub client: String,
    /// The session the client has open, or `None` (null or absent) on a page
    /// that shows none (the session list).
    pub session: Option<String>,
    /// Whether the person is attending: the page is visible, focused, and has
    /// seen recent input.
    pub active: bool,
}
