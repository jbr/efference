//! Mounting connected providers at boot, including the authorization dance
//! (docs/mcp-auth.md, docs/tools.md).
//!
//! Separate from `serve.rs` because this is the one place the harness stops and
//! waits for a human mid-boot, and that deserves to be readable on its own.
//!
//! ## The sequence per server
//!
//! 1. Mount with whatever credentials are stored — refreshing a stale grant
//!    first, since one round trip beats a `401` plus full rediscovery.
//! 2. If that comes back [`AuthorizationRequired`], run the dance and mount
//!    again with the new token.
//! 3. Anything else, or a second failure, is a mount failure like any other:
//!    fatal unless the stanza is `optional = true`.
//!
//! Only session preparation (`crate::boot`) calls this. `efference dev spotter --tool` mounts through
//! the plain path, so a spotter check can never open a browser — it fails with
//! a message pointing here instead.

use std::io::IsTerminal;

use crate::mcp::Unmounted;
use crate::mcp::auth::store::TokenStore;
use crate::mcp::auth::{self, AuthorizationRequired};
use crate::tools::Toolbox;
use trillium_client::Url;

/// Told which server an authorization waits on as the wait begins, and `None` as it
/// ends — how a starting session names the server in its state (docs/control-api.md).
pub type AuthorizationWaiting = std::sync::Arc<dyn Fn(Option<&str>) + Send + Sync>;

/// How a mount meets a server that asks for authorization (docs/mcp-auth.md).
/// The default runs the dance and tells nobody.
#[derive(Clone, Default)]
pub struct AuthorizationHandling {
    /// Fail the server's mount, naming why, instead of starting the dance: for a
    /// preparation nobody may be at the host to finish, where several sessions would
    /// otherwise each open a browser for one server.
    pub refuse: bool,
    /// Told about each wait the dance enters.
    pub waiting: Option<AuthorizationWaiting>,
}

/// Mount every declared server, authorizing any that asks for it unless
/// `authorization` refuses.
///
/// Returns the servers that did not mount and were allowed not to. A server
/// that did not mount and was *not* optional returns `Err` — the operator
/// declared it, so the session should not start pretending it exists.
pub async fn mount_all(
    toolbox: &mut Toolbox,
    servers: &std::collections::BTreeMap<String, crate::config::McpServerEntry>,
    cwd: &std::path::Path,
    store: &dyn TokenStore,
    reauth: &[String],
    authorization: &AuthorizationHandling,
) -> Result<Vec<Unmounted>, Box<dyn std::error::Error + Send + Sync>> {
    let mut unmounted = Vec::new();
    for (name, entry) in servers {
        // `--reauth <name>`: drop the stored credentials first, so the dance
        // runs even though something usable is on disk. The recovery path for a
        // revoked grant or a changed authorization server.
        if reauth.iter().any(|r| r == name) {
            store.clear(name)?;
            log::debug!("mcp: `{name}` credentials cleared; re-authorizing");
        }

        let reason = match mount_one(toolbox, name, entry, cwd, store, authorization).await {
            Ok(()) => continue,
            Err(e) => format!("{e:#}"),
        };
        if !entry.optional {
            // The reason and the fix ride the returned error — the create
            // response is where the operator reads it, and no print repeats it.
            return Err(format!(
                "mcp server `{name}` is declared in the configuration but did not mount: \
                 {reason}. Fix it, or set `optional = true` under [mcp.servers.{name}] to \
                 start without it."
            )
            .into());
        }
        // Optional: the skip is recorded in the mount result the session reports.
        log::debug!(
            "mcp: `{name}` did not mount and is optional — continuing without it: {reason}"
        );
        unmounted.push(Unmounted {
            name: name.clone(),
            reason,
        });
    }
    Ok(unmounted)
}

/// One server: mount, and authorize first if the server says to.
async fn mount_one(
    toolbox: &mut Toolbox,
    name: &str,
    entry: &crate::config::McpServerEntry,
    cwd: &std::path::Path,
    store: &dyn TokenStore,
    authorization: &AuthorizationHandling,
) -> anyhow::Result<()> {
    let header = auth::stored_header(store, name).await?;
    let first = toolbox
        .mount_connected(name, &entry.transport, cwd, header.as_deref())
        .await;
    let Err(e) = first else { return Ok(()) };

    // Only a `401` is worth a browser. Everything else — an unreachable host, a
    // TLS failure, a server that will not speak MCP — is just a failure.
    let Some(required) = e.downcast_ref::<AuthorizationRequired>() else {
        return Err(e);
    };
    let challenge = required.challenge.clone();
    let url = match &entry.transport {
        crate::config::McpTransport::Http { url } => url.clone(),
        // Unreachable in practice: a stdio server has no way to answer `401`.
        crate::config::McpTransport::Stdio { .. } => return Err(e),
    };

    if authorization.refuse {
        anyhow::bail!(
            "it asks for authorization, which this preparation does not start; resume the \
             session from a client to authorize it"
        );
    }
    eprintln!("[mcp: `{name}` requires authorization]");
    if let Some(waiting) = &authorization.waiting {
        waiting(Some(name));
    }
    let authorized =
        auth::authorize_and_store(store, name, &url, challenge.as_ref(), |authorization_url| {
            present(name, authorization_url)
        })
        .await;
    if let Some(waiting) = &authorization.waiting {
        waiting(None);
    }
    let header = authorized?;
    eprintln!("[mcp: `{name}` authorized]");

    toolbox
        .mount_connected(name, &entry.transport, cwd, Some(&header))
        .await
}

/// Get the operator to the authorization URL: print it, and try to open it.
///
/// **The browser is attempted unconditionally**, rather than gated on a tty.
/// A tty check looks like the right test and is not: a wrapper that filters the
/// child's output makes stdout and stderr pipes without making the session any
/// less interactive. `op run`, which masks secrets in what it wraps, does
/// exactly this — so the first real authorization printed "no terminal
/// attached" to a human sitting at a keyboard, and declined to open their
/// browser.
///
/// Attempting always is safe because the URL is printed either way: on a
/// genuinely headless host `webbrowser::open` returns an error, which is
/// reported, and the printed URL was already the answer. The failure modes are
/// not symmetric — a spurious open attempt costs a line of output, while a
/// spurious *refusal* leaves someone staring at a stalled boot.
fn present(server: &str, url: &Url) -> anyhow::Result<()> {
    // Kept only as a diagnostic. Nothing branches on it — see above.
    log::debug!(
        "presenting authorization; tty: stdin={} stdout={} stderr={}",
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        std::io::stderr().is_terminal()
    );
    eprintln!();
    eprintln!("  Authorize efference to use the `{server}` MCP server:");
    eprintln!();
    // The bare URL on its own line, unadorned, so a copy-paste out of a log or
    // a `tee` gets something that works. The hyperlink is *additional*: OSC 8
    // is least likely to be honored exactly where the output is being captured.
    eprintln!("  {}", osc8(url.as_str()));
    eprintln!();
    match webbrowser::open(url.as_str()) {
        Ok(()) => eprintln!("  (opened in your browser — waiting for you to finish)"),
        Err(e) => eprintln!("  (could not open a browser: {e} — open the URL above to continue)"),
    }
    eprintln!();
    Ok(())
}

/// An OSC 8 hyperlink whose visible text is the URL itself, so a terminal that
/// ignores the escape still shows something copy-pasteable.
fn osc8(url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\")
}

#[cfg(test)]
mod test;
