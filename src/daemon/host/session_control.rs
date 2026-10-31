//! The in-session `agents__*` provider a hosted session mounts (docs/tools.md), built by
//! what the process serving the host installs.
//!
//! The verbs are declared in `control_tools`, beside the command line's and `efference
//! mcp`'s, and that module sits above the host. So the host names none of them: whoever
//! serves it installs a [`MountSessionControl`] ([`Host::set_session_control`]) before
//! anything is adopted, and boot asks the host for the server a session mounts
//! ([`Host::session_control_server`]). A host with none installed boots sessions without
//! the `agents__*` tools.

use super::*;
use crate::tools::ControlServer;

impl Host {
    /// Install what builds the in-session server, before the first session boots. The
    /// first install stands.
    pub fn set_session_control(&self, mount: MountSessionControl) {
        let _ = self.session_control.set(mount);
    }

    /// The in-session server for the session `caller` (docs/tools.md): built over this
    /// host with `caller` as the identity its calls carry. `None` when no mount was
    /// installed.
    pub(crate) fn session_control_server(
        self: &Arc<Self>,
        caller: &str,
    ) -> Option<Box<dyn ControlServer>> {
        let mount = self.session_control.get()?;
        Some(mount(self, caller.to_string()))
    }
}
