//! The display flags of the dev verbs that draw a TUI (`view`, `gallery`): each
//! one given overrides its key in the user configuration's `[tui]` table
//! (docs/tui.md), so an instrument can be pointed at a theme or an image
//! protocol for one run.

use efference::inference::TuiConfig;

/// The `--images` value — the CLI face of [`efference::inference::ImageMode`]
/// (docs/tui.md). Detection lies under tmux in both directions, so the
/// override covers both: forcing a protocol detection missed, and turning off
/// one it wrongly claimed.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ImagesArg {
    /// Query the terminal; halfblocks when nothing answers.
    Auto,
    /// Force the kitty graphics protocol.
    Kitty,
    /// Force iTerm2's OSC 1337.
    Iterm2,
    /// No image rendering — an image shows as its one-line reference.
    Off,
}

impl From<ImagesArg> for efference::inference::ImageMode {
    fn from(arg: ImagesArg) -> Self {
        use efference::inference::ImageMode;
        match arg {
            ImagesArg::Auto => ImageMode::Auto,
            ImagesArg::Kitty => ImageMode::Kitty,
            ImagesArg::Iterm2 => ImageMode::Iterm2,
            ImagesArg::Off => ImageMode::Off,
        }
    }
}

/// `config` (the `[tui]` table) with each given flag laid over its key. A key no flag
/// names keeps its configured value.
pub(crate) fn over(
    mut config: TuiConfig,
    theme: Option<String>,
    images: Option<ImagesArg>,
    frame_cost: bool,
) -> TuiConfig {
    config.theme = theme.or(config.theme);
    if let Some(images) = images {
        config.images = images.into();
    }
    config.frame_cost |= frame_cost;
    config
}
