//! The session-control provider as the toolbox holds it (docs/tools.md): the verbs that
//! reach the host a session lives in, mounted under `agents__<verb>`.
//!
//! The verbs and the executor that runs them sit above the toolbox, so the toolbox names
//! neither. It mounts any [`SessionControl`] and holds the in-process server it builds
//! over one as a [`ControlServer`], the one provider whose type is erased. A server built
//! elsewhere is mounted as it comes ([`Toolbox::mount_control_server`]), which is how a
//! host that cannot name the verbs gets them into a session's toolbox.

use super::*;
use std::pin::Pin;

use crate::protocol::agents::BoundAction;

/// The session-control verbs' state, as a session's toolbox mounts it (docs/tools.md): the
/// verb set the toolbox lists, and the two answers the permission path takes from the
/// host before it classifies one of their calls (docs/permissions.md). The in-session
/// executor implements it, over the host the calling session lives in.
pub trait SessionControl: Clone + Send + Sync + 'static {
    /// The verbs, mounted under `agents__<verb>`.
    type Verbs: AsToolsList + Dispatch<Self>;

    /// Whether `call` is this session archiving its own predecessor, by the host's record
    /// of the hand-off (docs/permissions.md).
    fn completes_hand_off(&self, call: &ToolCall) -> bool;

    /// Bind an allowing `agents__decide` call to the leaf action its target holds
    /// (docs/permissions.md).
    fn resolve_bound_action(&self, call: &ToolCall) -> anyhow::Result<BoundAction>;
}

/// An [`McpServer`] over a [`SessionControl`], its types erased, so that
/// `Provider::Sessions` names neither the verbs nor their state (docs/tools.md). A call's
/// future is boxed here, which no other provider's is.
pub trait ControlServer: Send + Sync {
    /// The wire definitions, namespaced.
    fn definitions(&self) -> &[Value];
    /// Drop the tools `enabled` refuses; whether any remain.
    fn retain_enabled(&mut self, enabled: &dyn Fn(&str) -> bool) -> bool;
    /// Whether this server owns the named tool.
    fn handles(&self, name: &str) -> bool;
    /// This server's entry for `name`.
    fn schema(&self, name: &str) -> Option<&ToolDefinition>;
    /// Dispatch one call.
    fn run_call<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
    ) -> Pin<Box<dyn Future<Output = RawResult> + Send + 'a>>;
    /// [`SessionControl::completes_hand_off`], asked of the mounted state.
    fn completes_hand_off(&self, call: &ToolCall) -> bool;
    /// [`SessionControl::resolve_bound_action`], asked of the mounted state.
    fn resolve_bound_action(&self, call: &ToolCall) -> anyhow::Result<BoundAction>;
}

/// The in-process server over `control`, its types erased: the namespaced verb set, ready to
/// mount (docs/tools.md).
pub fn control_server<Control: SessionControl>(control: Control) -> Box<dyn ControlServer> {
    let server: McpServer<Control, Control::Verbs> =
        McpServer::from_state_namespaced(control, &[], crate::names::tools::AGENTS_NAMESPACE);
    Box::new(server)
}

impl<Control: SessionControl> ControlServer for McpServer<Control, Control::Verbs> {
    fn definitions(&self) -> &[Value] {
        &self.definitions
    }

    fn retain_enabled(&mut self, enabled: &dyn Fn(&str) -> bool) -> bool {
        McpServer::retain_enabled(self, &enabled)
    }

    fn handles(&self, name: &str) -> bool {
        McpServer::handles(self, name)
    }

    fn schema(&self, name: &str) -> Option<&ToolDefinition> {
        McpServer::schema(self, name)
    }

    fn run_call<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
    ) -> Pin<Box<dyn Future<Output = RawResult> + Send + 'a>> {
        Box::pin(McpServer::run_call(self, name, input))
    }

    fn completes_hand_off(&self, call: &ToolCall) -> bool {
        self.state.completes_hand_off(call)
    }

    fn resolve_bound_action(&self, call: &ToolCall) -> anyhow::Result<BoundAction> {
        self.state.resolve_bound_action(call)
    }
}
