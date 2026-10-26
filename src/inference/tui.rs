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

/// `[tui]`: how the terminal UI draws. Read by each terminal client, not by the host.
// `[tui]` as written. Every key is optional. The `///` docs are the published user schema's
// hover text (docs/config-schemas.md).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
#[schemars(rename = "Tui")]
pub struct TuiConfig {
    /// The color theme: a built-in name such as `"Catppuccin Frappe"`, `"Nord"`, or
    /// `"gruvbox-dark"`, or the path to a `.tmTheme` file. An unknown name lists the
    /// built-in ones at launch, and a leading `~/` in a path is your home directory.
    /// Default `"Catppuccin Frappe"`.
    pub theme: Option<String>,
    // Its hover text is `ImageMode`'s: Taplo ignores a description beside a `$ref`.
    #[serde(default)]
    pub images: ImageMode,
    /// Time each paint and print the distribution on exit. Development instrumentation
    /// for the streaming render.
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
