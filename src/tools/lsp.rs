//! The `lsp` built-ins (docs/tools.md): structural-lsp's `definition`, `references` and
//! `rename`, served in-process under `lsp__<tool>` against the language servers the host
//! shares among its sessions.

use super::*;
use std::sync::Arc;

use structural_lsp::host::AgentId;
use structural_lsp::session::Session;
use structural_lsp::tools::Tools;

use crate::record::names::tools::{LSP_NAMESPACE, NAMESPACE_SEPARATOR};

/// The host-wide language-server host: its workspaces are keyed by project root, so the
/// sessions in one project share one set of servers.
pub use structural_lsp::host::Host as LanguageServers;

/// Every tool the namespace serves, by wire name.
pub fn wire_names() -> impl Iterator<Item = String> {
    <Tools as AsToolsList>::tools_list()
        .into_iter()
        .map(|tool| format!("{LSP_NAMESPACE}{NAMESPACE_SEPARATOR}{}", tool.name))
}

/// Whether `role` mounts any `lsp` tool. Boot asks before binding, since binding starts the
/// project's language servers.
pub fn enabled(role: &crate::config::Config) -> bool {
    wire_names().any(|name| role.tool_enabled(&name))
}

/// The mounted provider: structural's tool set over one session's binding, and the
/// session's interest in that binding's project root.
///
/// Field order is drop order. The server drops its `Session` first, which stops nothing
/// while the interest still holds the workspace; the interest drops second and releases it.
pub struct Lsp {
    pub(super) server: McpServer<Session, Tools>,
    _interest: Interest,
}

impl Lsp {
    /// Bind session `id` at `cwd`, its working directory. Binding finds or creates the
    /// workspace for the project root above `cwd` and starts its servers without waiting
    /// for them; finding the root reads the filesystem, so it runs on the blocking pool.
    pub async fn bind(
        servers: Arc<LanguageServers>,
        id: &str,
        cwd: PathBuf,
    ) -> anyhow::Result<Self> {
        let agent = AgentId::new(id);
        let session = tokio::task::spawn_blocking({
            let servers = Arc::clone(&servers);
            let agent = agent.clone();
            move || servers.bind(agent, &cwd)
        })
        .await?;
        Ok(Self {
            server: McpServer::from_state_namespaced(session, &[], LSP_NAMESPACE),
            _interest: Interest { servers, agent },
        })
    }

    /// Run one call. A query can wait minutes on a server's first index, so unlike the
    /// other embedded servers this one races `abort`: a kill abandons the call, whose body
    /// finishes on the blocking pool holding the workspace until it does.
    pub(super) async fn run(
        &self,
        call: &ToolCall,
        abort: impl Future<Output = Interrupt> + Send,
    ) -> RawResult {
        let run = self.server.run_call(&call.name, &call.input);
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => result,
            interrupt = abort => match interrupt {
                Interrupt::Kill => RawResult::error(format!(
                    "`{}` call cancelled by the operator",
                    call.name
                )),
                // Detach is bash-only and keeps waiting.
                Interrupt::Detach => run.await,
            },
        }
    }
}

/// One session's interest in the workspace it bound. Dropping it ends the interest; when
/// it was the last on that project root, the root's servers stop, and stopping joins their
/// threads with a bounded grace — so the release runs on the blocking pool, not on
/// whichever worker dropped the toolbox.
struct Interest {
    servers: Arc<LanguageServers>,
    agent: AgentId,
}

impl Drop for Interest {
    fn drop(&mut self) {
        let servers = Arc::clone(&self.servers);
        let agent = self.agent.clone();
        let release = move || servers.agent_ended(&agent);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(release)),
            Err(_) => release(),
        }
    }
}

/// How long [`shut_down`] waits for admitted calls and graceful server exits before it
/// forces the servers.
const FORCE_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// The host's language servers, reporting to the log. Constructing them starts nothing:
/// a workspace and its servers start when a session binds.
pub fn language_servers() -> Arc<LanguageServers> {
    Arc::new(LanguageServers::new().with_operator(Arc::new(LogOperator)))
}

/// Stop every language server `servers` started: close admission, wait on the blocking
/// pool for admitted calls and every child's reaping, and force the servers when that takes
/// longer than [`FORCE_AFTER`]. A forced call fails rather than answering partly.
pub async fn shut_down(servers: Arc<LanguageServers>) {
    servers.request_shutdown();
    let wait = tokio::task::spawn_blocking({
        let servers = Arc::clone(&servers);
        move || servers.wait_shutdown()
    });
    tokio::pin!(wait);
    if tokio::time::timeout(FORCE_AFTER, &mut wait).await.is_err() {
        log::warn!("language servers still stopping after {FORCE_AFTER:?}; forcing them");
        servers.force_shutdown();
        let _ = wait.await;
    }
}

/// structural's operator events — a server that would not start, a fallback, a language
/// nothing serves, a server released, a watch lost — logged at info.
struct LogOperator;

impl structural_lsp::operator::Operator for LogOperator {
    fn event(&self, event: &structural_lsp::operator::Event) {
        log::info!("{event}");
    }
}

#[cfg(test)]
mod test;
