//! The `[tui]` table of the user configuration: how a terminal client draws
//! (docs/tui.md).
//!
//! It lives in the user configuration rather than on a command line because
//! the TUI is drawn by more than one surface — bare `efference`, the island it
//! starts, the session a hand-off moves it to — and each reads the same table.
//! The host never reads it.

use std::path::Path;

use serde::Deserialize;

use crate::api::Error;
use crate::tui::ImageMode;

/// `[tui]` as written. Every key is optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TuiConfig {
    /// Syntax/UI theme: a built-in theme name (e.g. "Catppuccin Frappe",
    /// "Nord", "gruvbox-dark") or a path to a `.tmTheme` file. An unknown name
    /// lists the available ones at launch. Omitted, "Catppuccin Frappe".
    pub theme: Option<String>,
    /// Result-image rendering: `"auto"` queries the terminal, `"kitty"` and
    /// `"iterm2"` force a protocol detection missed (tmux), `"off"` disables.
    #[serde(default)]
    pub images: ImageMode,
    /// Time each paint and print the distribution on exit: development
    /// instrumentation for the streaming render.
    #[serde(default)]
    pub frame_cost: bool,
}

impl TuiConfig {
    /// The table from the user configuration under `home`. A missing file is an
    /// empty table, and a theme path written `~/…` is expanded against `home`.
    pub fn load(home: &Path) -> Result<Self, Error> {
        let config = super::InferenceConfig::read_user(home)?.unwrap_or_default();
        Ok(config.tui.expand_home(home))
    }

    fn expand_home(mut self, home: &Path) -> Self {
        self.theme = self.theme.map(|theme| match theme.strip_prefix("~/") {
            Some(rest) => home.join(rest).display().to_string(),
            None => theme,
        });
        self
    }
}

#[cfg(test)]
mod test;
