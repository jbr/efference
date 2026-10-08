//! What the operator has actually overridden (docs/tui.md).
//!
//! The draft is the form's own state and the only thing creation sends. The
//! host's `effective` view is a separate answer *about* this draft — it says
//! what a session would get and which precedence branch supplied it — and never
//! replaces it: folding resolved values back into the draft would turn every
//! inherited default into an explicit override on the first preview
//! (docs/control-api.md).

use crate::inference::{SessionOptions, TriageChoice};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Main,
    Spotter,
    Triage,
}

impl Role {
    /// How the role reads in both the form row and the picker question.
    /// Lower-case, because the whole surface speaks in the picker's voice
    /// (`session_form/render.rs`) and one spelling keeps the two in step.
    /// The role as the catalog's model rule knows it.
    pub(in crate::tui) fn model_role(self) -> crate::client::ModelRole {
        match self {
            Self::Main => crate::client::ModelRole::Main,
            Self::Spotter => crate::client::ModelRole::Spotter,
            Self::Triage => crate::client::ModelRole::Triage,
        }
    }

    pub(in crate::tui) fn label(self) -> &'static str {
        match self {
            Self::Main => "main model",
            Self::Spotter => "spotter model",
            Self::Triage => "triage model",
        }
    }
}

/// One role's override: the connection by name, and the provider-native model id
/// under it. Two fields rather than a `connection/model` slug, because that is
/// what the request shape names and what keeps a native id containing a slash
/// unambiguous (docs/control-api.md).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct RoleDraft {
    pub(in crate::tui) connection: String,
    pub(in crate::tui) model: String,
}

/// The triage control has two states: inheriting the main client, or naming a
/// separate one. With no configured triage default, "nothing said" always
/// resolves to sharing main.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::tui) enum TriageDraft {
    #[default]
    Inherit,
    Separate(RoleDraft),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::tui) struct Draft {
    pub(in crate::tui) main: Option<RoleDraft>,
    pub(in crate::tui) spotter: Option<RoleDraft>,
    pub(in crate::tui) triage: TriageDraft,
    pub(in crate::tui) effort: Option<String>,
    pub(in crate::tui) temperature: Option<u8>,
}

impl Draft {
    pub(in crate::tui) fn role(&self, role: Role) -> Option<&RoleDraft> {
        match role {
            Role::Main => self.main.as_ref(),
            Role::Spotter => self.spotter.as_ref(),
            Role::Triage => match &self.triage {
                TriageDraft::Separate(draft) => Some(draft),
                TriageDraft::Inherit => None,
            },
        }
    }

    /// Record a role's selection, or clear it back to inheriting — which for
    /// triage is sharing the main client.
    pub(in crate::tui) fn set_role(&mut self, role: Role, selection: Option<RoleDraft>) {
        match role {
            Role::Main => self.main = selection,
            Role::Spotter => self.spotter = selection,
            Role::Triage => {
                self.triage = match selection {
                    Some(draft) => TriageDraft::Separate(draft),
                    None => TriageDraft::Inherit,
                }
            }
        }
    }

    /// The draft as the control routes accept it. Preview and creation take the
    /// same selections, so this one conversion feeds both and they cannot drift
    /// (docs/control-api.md).
    pub(in crate::tui) fn to_options(&self) -> SessionOptions {
        let (connection, model) = split(self.main.as_ref());
        let (spotter_connection, spotter_model) = split(self.spotter.as_ref());
        SessionOptions {
            connection,
            model,
            spotter_connection,
            spotter_model,
            triage: match &self.triage {
                // Inheriting sends nothing, so triage shares the main client.
                TriageDraft::Inherit => None,
                TriageDraft::Separate(draft) => Some(TriageChoice::Separate {
                    connection: Some(draft.connection.clone()),
                    model: draft.model.clone(),
                    thinking: false,
                    auto_choice: false,
                    effort: None,
                }),
            },
            effort: self.effort.clone(),
            temperature: self.temperature,
        }
    }
}

fn split(draft: Option<&RoleDraft>) -> (Option<String>, Option<String>) {
    match draft {
        Some(draft) => (Some(draft.connection.clone()), Some(draft.model.clone())),
        None => (None, None),
    }
}
