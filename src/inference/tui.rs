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

/// How the terminal UI draws images in tool results. Default `"auto"`.
// How the TUI picks its graphics protocol for result images (docs/tui.md). Detection is a
// startup act — `Picker::from_query_stdio` runs once in `ui_loop`, before the crossterm reader
// thread takes stdin — and tmux makes detection lie in both directions (its DA1 masks a
// kitty-capable outer terminal; passthrough can make OSC 1337 work with nothing detectable
// saying so), so the override ships in the same change as detection. Set by `images` under
// `[tui]` in the user configuration, spelled in lowercase; the `///` docs are its published
// hover text (docs/config-schemas.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImageMode {
    /// Ask the terminal which graphics protocol it supports, and fall back to half-block
    /// characters when nothing answers.
    #[default]
    Auto,
    /// Use the kitty graphics protocol, for a kitty terminal that tmux hides from
    /// detection.
    Kitty,
    /// Use iTerm2's inline image protocol, for a terminal detection missed.
    // Env identification is weaker than kitty's query fence, so forcing it is sometimes the
    // truth detection missed.
    Iterm2,
    /// Show each image as a one-line reference.
    Off,
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
