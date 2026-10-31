//! The selections a resume may change: the spotter, triage and main models
//! (docs/resumption.md).

use serde::{Deserialize, Serialize};

use super::{SessionOptions, TriageChoice};

/// The model selections an `active` patch may carry, in the shapes creation takes
/// (docs/control-api.md). A field left out keeps what the session recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ResumeSwitch {
    /// The main model: a connection and a native model id, or the `connection/model`
    /// slug as `model` alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spotter_connection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spotter_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<TriageChoice>,
}

impl ResumeSwitch {
    /// Overlay the selections this switch names onto `options`. A role's
    /// connection and model are one selection, so naming either replaces both; a
    /// connection with no model is then an issue the resolver reports, as it is at
    /// creation.
    pub fn apply(&self, options: &mut SessionOptions) {
        if self.connection.is_some() || self.model.is_some() {
            options.connection = self.connection.clone();
            options.model = self.model.clone();
        }
        if self.spotter_connection.is_some() || self.spotter_model.is_some() {
            options.spotter_connection = self.spotter_connection.clone();
            options.spotter_model = self.spotter_model.clone();
        }
        if let Some(triage) = &self.triage {
            options.triage = Some(triage.clone());
        }
    }

    /// Whether the switch names a main model.
    pub fn switches_main(&self) -> bool {
        self.connection.is_some() || self.model.is_some()
    }

    /// Whether the switch names nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}
