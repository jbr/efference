//! The `efference dev <verb>` group: the instruments for developing efference
//! itself (docs/operating.md), compiled only under the `dev` cargo feature.
//!
//! None of these drive a session. They read a recorded log, render the wire a turn
//! would send, replay recorded gates, exercise the spotter rung directly, fold
//! `usage.jsonl` into a cache read rate, open a recorded log read-only in the TUI, or
//! mint a bootstrap link for the web bind —
//! the views and testers that make the harness's own behaviour inspectable. The feature
//! is in `default` because a gated feature nobody builds rots; a release build
//! passes `--no-default-features`.

use super::*;

mod args;
mod command;
mod display;
mod launch;
mod run;
#[cfg(test)]
mod test;
mod wire;

pub(crate) use args::*;
pub(crate) use command::DevCommand;
pub(crate) use run::run;

use display::ImagesArg;
use launch::{gallery, open, view};
use wire::wire;
#[cfg(test)]
use wire::{LEGACY_WIRE_MODEL, wire_model, wire_placements};
