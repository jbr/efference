//! The role selections a resume may change (docs/resumption.md).

use serde::{Deserialize, Serialize};

use super::{SessionOptions, TriageChoice};

/// The spotter and triage selections an `active` patch may carry, in the shapes
/// creation takes (docs/control-api.md). A field left out keeps what the session
/// recorded; the main model is not among them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeSwitch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spotter_connection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spotter_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<TriageChoice>,
}

impl ResumeSwitch {
    /// Overlay the selections this switch names onto `options`. The spotter's
    /// connection and model are one selection, so naming either replaces both; a
    /// connection with no model is then an issue the resolver reports, as it is at
    /// creation.
    pub fn apply(&self, options: &mut SessionOptions) {
        if self.spotter_connection.is_some() || self.spotter_model.is_some() {
            options.spotter_connection = self.spotter_connection.clone();
            options.spotter_model = self.spotter_model.clone();
        }
        if let Some(triage) = &self.triage {
            options.triage = Some(triage.clone());
        }
    }

    /// Whether the switch names nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}
