//! Model selection for the verbs that dial a provider without a host: the same
//! `~/.efference/config.toml` and startup environment a host loads
//! (docs/transport.md), resolved once per invocation.

use efference::api::{Endpoint, Error};
use efference::inference::{InferenceRegistry, SessionInference, TriageChoice};

/// Load the operator's connections from this process's environment.
pub(crate) fn registry() -> Result<InferenceRegistry, Error> {
    let cwd = std::env::current_dir()?;
    let env = efference::env_table::EnvSnapshot::capture()?;
    let secrets = std::sync::Arc::new(efference::secrets::Secrets::detect()?);
    InferenceRegistry::load(&cwd, env, &secrets)
}

/// Resolve the roles a verb names, as a session creation would.
pub(crate) fn resolve(
    model: Option<&str>,
    spotter: Option<&str>,
    triage: Option<TriageChoice>,
) -> Result<SessionInference, Error> {
    registry()?.resolve(model, spotter, triage)
}

/// The main role's native model and endpoint for a `--model` selection.
pub(crate) fn main_endpoint(model: &str) -> Result<(String, Endpoint), Error> {
    let selected = resolve(Some(model), None, None)?;
    let (selection, endpoint) = selected.main();
    Ok((selection.model.clone(), endpoint))
}

/// The spotter role's native model and endpoint: the named selection, or the
/// configured default.
pub(crate) fn spotter_endpoint(model: Option<&str>) -> Result<(String, Endpoint), Error> {
    let selected = resolve(None, model, None)?;
    let (selection, endpoint) = selected.spotter();
    Ok((selection.model.clone(), endpoint))
}
