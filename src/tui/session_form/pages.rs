//! Where each picker and editor sends the draft when it is committed
//! (docs/tui.md).
//!
//! Every method here ends one page: it folds the selection into the [`Draft`],
//! returns to the form, and asks for a fresh preview. That is the whole reason
//! the request rate follows selections rather than keystrokes — the typing that
//! happens *inside* a page never reaches these. The model picker's commit is in
//! `picker.rs`, which calls [`Form::set_role_model`].

use super::*;

impl Form {
    pub(super) fn accept_temperature(&mut self) -> Action {
        let Page::Temperature { value } = &self.page else {
            return Action::Continue;
        };
        let value = value.text().trim().to_string();
        if value.is_empty() {
            self.draft.temperature = None;
        } else {
            match value.parse::<u8>() {
                Ok(n) if crate::inference::TEMPERATURE.contains(&n) => {
                    self.draft.temperature = Some(n);
                }
                _ => {
                    self.note = Some(format!(
                        "Use {}–{}, or leave empty for the provider default.",
                        crate::inference::TEMPERATURE.start(),
                        crate::inference::TEMPERATURE.end()
                    ));
                    return Action::Continue;
                }
            }
        }
        self.page = Page::Form;
        self.note = None;
        self.preview()
    }

    /// Fold a picked model into the draft for `role`. The connection is named,
    /// not indexed: the picker's rows carry the name the host gave them.
    pub(super) fn set_role_model(&mut self, role: Role, connection: &str, model: String) {
        self.draft.set_role(
            role,
            Some(RoleDraft {
                connection: connection.to_string(),
                model,
            }),
        );
    }
}
