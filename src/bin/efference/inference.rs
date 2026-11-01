//! Model selection for the verbs that dial a provider without a host: the same
//! `~/.efference/config.toml` and startup environment a host loads
//! (docs/transport.md), resolved once per invocation.

use efference::api::{Endpoint, Error};
use efference::inference::{InferenceRegistry, SessionInference, TriageChoice};

/// Load the operator's connections from this process's environment, with the
/// model catalog the host stores beside them, so a default the catalog decides
/// (the spotter's, docs/model-catalog.md) is the one a host session would get.
pub(crate) fn registry() -> Result<InferenceRegistry, Error> {
    let cwd = std::env::current_dir()?;
    let env = efference::env_table::EnvSnapshot::capture()?;
    let secrets = std::sync::Arc::new(efference::platform::secrets::Secrets::configured(
        efference::user_config::InferenceConfig::user_secret_commands()?,
    ));
    let registry = InferenceRegistry::load(&cwd, &env, &secrets)?;
    let catalog =
        efference::catalog::Catalog::new(registry.history().state_path().map(ToOwned::to_owned));
    Ok(registry.with_catalog(catalog))
}

/// Resolve the roles a verb names, as a session creation would.
pub(crate) fn resolve(
    model: Option<&str>,
    spotter: Option<&str>,
    triage: Option<TriageChoice>,
) -> Result<SessionInference, Error> {
    registry()?.resolve(model, spotter, triage)
}

/// The spotter role's native model and endpoint: the named selection, or the
/// configured default. Only the spotter is resolved, so a config with no main
/// default still runs the spotter verbs.
pub(crate) fn spotter_endpoint(model: Option<&str>) -> Result<(String, Endpoint), Error> {
    let (selection, endpoint) = registry()?.resolve_spotter_role(model)?;
    Ok((selection.model, endpoint))
}
