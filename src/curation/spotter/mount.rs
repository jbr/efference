//! Which spotter a session runs (docs/permissions.md).
//!
//! Two implementations of the rung exist — [`ForkSpot`] against a generative
//! model, [`JevSpot`] against a decision model — and they are not
//! interchangeable at the model layer: the decisions route is a different API
//! with a different request shape and a different kind of answer. The selection
//! is therefore made once, at mount, from the [`ModelKind`] the provider reports
//! for the spotter model (`Endpoint::spotter_kind`), and the loop holds the
//! result behind the [`Spotter`] trait.
//!
//! The kind is the provider's own statement about the model — a decision model
//! declares its output modality — rather than a naming convention, so a model
//! released after this build is classified correctly.
//!
//! An enum rather than a boxed trait object because the trait is not object-safe
//! (its method returns an `impl Future`) and needn't be: one rung is mounted per
//! session, chosen once.

use crate::config::Config;
use crate::inference::api::{Endpoint, ModelKind};
use crate::record::log::ToolCall;
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
        kind: ModelKind,
        scratch: Option<std::path::PathBuf>,
        directories: crate::config::policy::Directories,
    ) -> Self {
        let mounted = match kind {
            ModelKind::Decisions => MountedSpotter::Jev(JevSpot::new(endpoint, model)),
            ModelKind::Text => MountedSpotter::Fork(ForkSpot::new(endpoint, model)),
        };
        mounted.with_scratch(scratch).with_directories(directories)
    }

    /// The prose rung over a replacement persona, or the decision rung, whichever
    /// `model` names — the offline eval's entry point (docs/permissions.md).
    /// A persona is meaningless to a decision model, which has no prompt to
    /// replace, so naming both is a misconfiguration rather than a silent no-op.
    pub fn mount_with_persona(
        endpoint: Endpoint,
        model: &str,
        kind: ModelKind,
        persona: Option<String>,
    ) -> Result<Self, crate::inference::api::Error> {
        match (kind == ModelKind::Decisions, persona) {
            (true, Some(_)) => Err(format!(
                "spotter model {model:?} is a decision model, which takes no persona: it \
                 answers fixed questions and has no prompt to replace. Its tuning knob is \
                 the sketch and precedent fold."
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

    /// Apply the model's thinking answer (`ModelCapabilities::thinking`) to the
    /// prose rung's client; a decision model sends no `thinking` field.
    pub fn with_thinking(self, thinking: crate::inference::api::ThinkingSupport) -> Self {
        match self {
            MountedSpotter::Fork(fork) => MountedSpotter::Fork(fork.with_thinking(thinking)),
            jev @ MountedSpotter::Jev(_) => jev,
        }
    }

    /// The session's project and working directories, for the spotter's scope.
    pub fn with_directories(self, directories: crate::config::policy::Directories) -> Self {
        match self {
            MountedSpotter::Fork(fork) => MountedSpotter::Fork(fork.with_directories(directories)),
            MountedSpotter::Jev(jev) => MountedSpotter::Jev(jev.with_directories(directories)),
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
    fn set_stage_sink(&mut self, sink: crate::inference::api::StageSink) {
        match self {
            MountedSpotter::Fork(fork) => fork.set_stage_sink(sink),
            MountedSpotter::Jev(jev) => jev.set_stage_sink(sink),
        }
    }

    async fn assess(
        &self,
        role: &Config,
        call: &ToolCall,
        brief: Option<&ToolBrief<'_>>,
        chain: &[String],
        probe: Option<&str>,
        precedents: &[ToolCall],
    ) -> Spotting {
        match self {
            MountedSpotter::Fork(fork) => {
                fork.assess(role, call, brief, chain, probe, precedents)
                    .await
            }
            MountedSpotter::Jev(jev) => {
                jev.assess(role, call, brief, chain, probe, precedents)
                    .await
            }
        }
    }
}
