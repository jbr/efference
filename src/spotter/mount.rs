//! Which spotter a session runs (docs/permissions.md).
//!
//! Two implementations of the rung exist — [`ForkSpot`] against a generative
//! model, [`JevSpot`] against a decision model — and they are not
//! interchangeable at the model layer: the decisions route is a different API
//! with a different request shape and a different kind of answer. The selection
//! is therefore made once, at mount, from the spotter model id alone
//! ([`jev::is_decision_model`]), and the loop holds the result behind the
//! [`Spotter`] trait as it always did.
//!
//! The id is the whole switch on purpose. A second flag saying "and it is a
//! decision model" would be a second thing to keep consistent with the first,
//! and every way they could disagree is a misconfiguration that the id alone
//! cannot have.
//!
//! An enum rather than a boxed trait object because the trait is not object-safe
//! (its method returns an `impl Future`) and needn't be: one rung is mounted per
//! session, chosen once.

use crate::api::Endpoint;
use crate::config::Config;
use crate::log::ToolCall;
use crate::tools::ToolBrief;

use super::{ForkSpot, Spotter, Spotting, jev::JevSpot};

/// The rung a session actually mounted.
// `large_enum_variant` wants the bigger arm boxed. Exactly one of these exists per
// session, built once at boot and held for the session's life — it is never in a
// collection and never moved on a hot path, so the ~450 bytes the smaller arm
// wastes buy nothing back and an indirection would only add a dereference to every
// assessment.
#[allow(clippy::large_enum_variant)]
pub enum MountedSpotter {
    Fork(ForkSpot),
    Jev(JevSpot),
}

impl MountedSpotter {
    /// Mount the rung `model` names, on `endpoint`.
    ///
    /// `endpoint` is the spotter's own — already under its distinct sticky key
    /// ([`Endpoint::for_spotter`]) — and the caller has already validated the
    /// model against its provider's metadata, so nothing here can fail: an id
    /// that isn't a decision model is a generative one by construction.
    pub fn mount(
        endpoint: Endpoint,
        model: &str,
        scratch: Option<std::path::PathBuf>,
        cwd: impl Into<std::path::PathBuf>,
    ) -> Self {
        let mounted = match super::jev::is_decision_spotter(endpoint.provider, model) {
            true => MountedSpotter::Jev(JevSpot::new(endpoint, model)),
            false => MountedSpotter::Fork(ForkSpot::new(endpoint, model)),
        };
        mounted.with_scratch(scratch).with_cwd(cwd)
    }

    /// The prose rung over a replacement persona, or the decision rung, whichever
    /// `model` names — the offline eval's entry point (docs/permissions.md).
    /// A persona is meaningless to a decision model, which has no prompt to
    /// replace, so naming both is a misconfiguration rather than a silent no-op.
    pub fn mount_with_persona(
        endpoint: Endpoint,
        model: &str,
        persona: Option<String>,
    ) -> Result<Self, crate::api::Error> {
        match (
            super::jev::is_decision_spotter(endpoint.provider, model),
            persona,
        ) {
            (true, Some(_)) => Err(format!(
                "spotter model {model:?} is a decision model, which takes no persona: it \
                 answers fixed questions and has no prompt to replace. Its tuning knob is \
                 the threshold band (docs/permissions.md)."
            )
            .into()),
            (true, None) => Ok(MountedSpotter::Jev(JevSpot::new(endpoint, model))),
            (false, Some(persona)) => Ok(MountedSpotter::Fork(ForkSpot::with_persona(
                endpoint, model, persona,
            ))),
            (false, None) => Ok(MountedSpotter::Fork(ForkSpot::new(endpoint, model))),
        }
    }

    pub fn with_scratch(self, scratch: Option<std::path::PathBuf>) -> Self {
        match self {
            MountedSpotter::Fork(fork) => MountedSpotter::Fork(fork.with_scratch(scratch)),
            MountedSpotter::Jev(jev) => MountedSpotter::Jev(jev.with_scratch(scratch)),
        }
    }

    pub fn with_cwd(self, cwd: impl Into<std::path::PathBuf>) -> Self {
        match self {
            MountedSpotter::Fork(fork) => MountedSpotter::Fork(fork.with_cwd(cwd)),
            MountedSpotter::Jev(jev) => MountedSpotter::Jev(jev.with_cwd(cwd)),
        }
    }

    /// What to call this rung in a log line or a status surface.
    pub fn kind(&self) -> &'static str {
        match self {
            MountedSpotter::Fork(_) => "prose",
            MountedSpotter::Jev(_) => "decisions",
        }
    }
}

impl Spotter for MountedSpotter {
    async fn assess(
        &self,
        role: &Config,
        call: &ToolCall,
        brief: Option<&ToolBrief<'_>>,
        chain: &[String],
        probe: Option<&str>,
    ) -> Spotting {
        match self {
            MountedSpotter::Fork(fork) => fork.assess(role, call, brief, chain, probe).await,
            MountedSpotter::Jev(jev) => jev.assess(role, call, brief, chain, probe).await,
        }
    }
}
