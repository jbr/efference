//! What a boot `Preamble` records besides the system text: the inference
//! identities the session ran on (docs/log-and-projection.md, docs/resumption.md).

use serde::{Deserialize, Serialize};

use super::{LogEntry, Payload, Provider, context};

/// The inference identity persisted in a boot [`Payload::Preamble`].
///
/// This is deliberately a log-owned shape rather than the control/configuration
/// selection type. Boot copies the three facts offline readers need into this
/// record, so a rename in the live configuration model does not change how an
/// existing log deserializes (docs/log-and-projection.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct LoggedInference {
    pub connection: String,
    pub provider: Provider,
    pub model: String,
}

impl LoggedInference {
    /// Construct the persisted identity from the three values recorded at boot.
    pub fn new(
        connection: impl Into<String>,
        provider: Provider,
        model: impl Into<String>,
    ) -> Self {
        Self {
            connection: connection.into(),
            provider,
            model: model.into(),
        }
    }
}

/// The inference identity recorded by the latest boot [`Payload::Preamble`], or
/// `None` for a log written before that metadata existed. The latest preamble is
/// the boot record for a multi-boot log; an older identity is not resurrected when
/// the latest preamble has none (docs/log-and-projection.md).
pub fn recorded_inference(entries: &[LogEntry]) -> Option<&LoggedInference> {
    entries
        .iter()
        .rev()
        .find_map(|entry| match &entry.payload {
            Payload::Preamble { inference, .. } => Some(inference.as_ref()),
            _ => None,
        })
        .flatten()
}

/// Which inference the triage step ran on (docs/triage.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum TriageModel {
    /// Triage ran on the session's main inference.
    Shared,
    /// Triage ran on its own inference, with the reasoning settings boot
    /// resolved for it — the knob set that inference is called with, recorded
    /// so a reader need not consult live configuration to reproduce the step.
    Separate {
        selection: LoggedInference,
        thinking: bool,
        auto_choice: bool,
        effort: Option<String>,
    },
}

impl TriageModel {
    /// Triage on the main inference.
    pub fn shared() -> Self {
        Self::Shared
    }

    /// Triage on `selection`, with `thinking` and `auto_choice` off and no
    /// effort override — the minimum a caller setting the other fields passes.
    pub fn separate(selection: LoggedInference) -> Self {
        Self::Separate {
            selection,
            thinking: false,
            auto_choice: false,
            effort: None,
        }
    }
}

/// The spotter and triage selections a `Preamble` records after a resume changed
/// them (docs/resumption.md). The header holds the roles a session was created
/// with; the latest `Preamble` that carries this overrides it, so a switched role
/// survives the next resume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct RecordedRoles {
    pub spotter: LoggedInference,
    pub triage: TriageModel,
}

/// The roles recorded by the latest [`Payload::Preamble`], or `None` when that
/// preamble carries none and the header's stand. As with [`recorded_inference`], an
/// older preamble's roles are not resurrected.
pub fn recorded_roles(entries: &[LogEntry]) -> Option<&RecordedRoles> {
    entries
        .iter()
        .rev()
        .find_map(|entry| match &entry.payload {
            Payload::Preamble { roles, .. } => Some(roles.as_ref()),
            _ => None,
        })
        .flatten()
}

impl Payload {
    /// A boot preamble recording no roles. The one construction site in
    /// production and in tests, so a field added to the entry lands here.
    pub fn preamble(
        identity: impl Into<String>,
        body: impl Into<String>,
        context: context::Resolved,
        inference: Option<LoggedInference>,
    ) -> Self {
        Payload::Preamble {
            identity: identity.into(),
            body: body.into(),
            context,
            inference,
            roles: None,
        }
    }

    /// The `system` string a preamble's two parts assemble to, or `None` for any
    /// other payload.
    pub fn preamble_system(&self) -> Option<String> {
        match self {
            Payload::Preamble { identity, body, .. } => Some(format!("{identity}{body}")),
            _ => None,
        }
    }

    /// This preamble with the main model replaced: a new identity line over the same
    /// body, and the new inference identity. Any other payload is returned as it was.
    pub fn with_main(mut self, new_identity: String, main: LoggedInference) -> Self {
        if let Payload::Preamble {
            identity,
            inference,
            ..
        } = &mut self
        {
            *identity = new_identity;
            *inference = Some(main);
        }
        self
    }

    /// This preamble with `roles` recorded; any other payload is returned as it was.
    pub fn with_roles(mut self, recorded: RecordedRoles) -> Self {
        if let Payload::Preamble { roles, .. } = &mut self {
            *roles = Some(recorded);
        }
        self
    }
}
