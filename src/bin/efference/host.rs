//! `efference host` — the machine's front door (docs/host.md).
//!
//! Flag resolution only: the bind and its credentials are `serve --bind`'s, validated
//! by the same [`resolve_bind`], so the two commands cannot disagree about what a
//! plaintext bind or a missing root secret means.

use super::*;
use serve::BindFlags;

pub(crate) async fn host(flags: BindFlags) -> CliResult {
    // The daemon's logging setup, for the same reason: RUST_LOG controls verbosity,
    // and the proxy logs which upstream a request went to at debug.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,efference=info"),
    )
    .try_init();
    // Unlike `serve`, where the bind is optional, a host with no listener has nothing
    // to be: refuse rather than starting a process that answers nowhere.
    let bind = serve::resolve_bind(flags)?.ok_or(
        "efference host needs --bind <addr:port> — it is the listener, not an addition to one",
    )?;
    let host = efference::host::Host::new(bind);
    eprintln!("sessions: {}", host.base.display());
    efference::host::serve(host).await?;
    Ok(())
}
