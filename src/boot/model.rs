//! The main model's boot checks: capability lookup, temperature, effort
//! (docs/transport.md).

use super::describe_window;
use crate::api::{Endpoint, Error, ModelCapabilities, UNKNOWN_OUTPUT_CAP};

/// Look up the main model's capabilities and check the session's sampling
/// temperature and effort level against them, reporting the result on the boot
/// line. `effort` is the level already settled from the flag or the config.
pub(super) async fn resolve_capabilities(
    endpoint: &Endpoint,
    model: &str,
    upstream: &[String],
    temperature: Option<u8>,
    effort: Option<&str>,
) -> Result<ModelCapabilities, Error> {
    // The required model-metadata lookup (docs/transport.md): capability conditionals
    // (the context ceiling now; more as they land) must not run on guesses, so a failed
    // lookup fails serve rather than starting blind.
    // The `--upstream` pin narrows which endpoints answer the question: a pinned
    // session's ceilings are the pinned endpoints', not the aggregate across every
    // upstream serving the slug (docs/transport.md).
    let capabilities = match endpoint.fetch_capabilities(model, upstream).await {
        Ok(caps) => caps,
        Err(e) => {
            return Err(format!(
                "model metadata lookup for {model:?} on {} failed: {e}",
                endpoint.provider.name()
            )
            .into());
        }
    };
    // A model that refuses the parameter fails here rather than on the first
    // request, where the rejection would read as a transport error.
    // (`describe_window` is defined below.)
    if temperature.is_some() && !capabilities.accepts_temperature {
        return Err(
            format!("--temperature: {model} does not accept a sampling temperature").into(),
        );
    }
    eprintln!(
        "model: {model} · context {}{}",
        describe_window(capabilities.context_length),
        match capabilities.max_output_tokens {
            Some(cap) => format!(" · max output {cap}"),
            // Said as an assumption rather than printed as a fact, so an
            // unreported cap is visible here instead of surprising later.
            None => format!(" · max output {UNKNOWN_OUTPUT_CAP} assumed (none reported)"),
        }
    );

    // The one cross-axis coupling is checked here rather than forwarded as the
    // provider's 400: `xhigh`/`max` need thinking, which a model whose metadata
    // offers no adaptive thinking takes away — the capability lookup above is what
    // makes the effective answer computable here.
    // Validate before the coupling check: the metadata's list is the level's
    // only defense, so an unsupported spelling is named first (docs/transport.md).
    if let Some(effort) = effort
        && let Err(why) = capabilities.check_effort(effort, model)
    {
        return Err(why.into());
    }
    if let Err(why) = crate::api::check_effort_thinking(
        effort,
        model,
        capabilities.thinking.auto_turns_send_thinking_off(),
    ) {
        return Err(why.into());
    }
    match (effort, &capabilities.default_effort) {
        (Some(effort), _) if capabilities.validates_efforts => eprintln!("effort: {effort}"),
        // Sent without a check against the model's metadata, which may name no
        // levels at all: the provider may honor it, refuse it, or drop it
        // silently, and only the last one leaves no trace (docs/transport.md).
        (Some(effort), _) => {
            eprintln!("effort: {effort} · unchecked, the model's metadata does not bound it");
        }
        (None, Some(default)) => {
            eprintln!("effort: unset · provider default {default}");
        }
        (None, None) => {}
    }
    Ok(capabilities)
}
