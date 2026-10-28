//! The `[updates]` table of the user configuration: whether a host checks for a newer
//! efference (docs/update-check.md).
//!
//! It lives beside the connections because the answer is the user's for the machine,
//! given once in `efference setup`. A file with no answer never checks: the check
//! contacts efference.net, which nothing else does, so it waits to be allowed rather
//! than being a default to find and turn off.

use std::path::Path;

use serde::Deserialize;

use crate::api::Error;

/// `[updates]`: whether efference checks for newer versions of itself.
// `[updates]` as written. The `///` docs are the published user schema's hover text
// (docs/config-schemas.md).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
#[schemars(rename = "Updates")]
pub struct UpdatesConfig {
    /// Ask efference.net when a host starts, and about once a day after, whether a
    /// newer version of efference is out, and say so in the terminal UI and web
    /// client. Nothing is installed. `efference setup` asks and writes the answer;
    /// with no answer, nothing is asked. `efference update --check` asks once either
    /// way.
    #[serde(default)]
    pub check: bool,
}

impl UpdatesConfig {
    /// The table from the user configuration under `home`. A missing file is an empty
    /// table, which checks nothing.
    pub fn load(home: &Path) -> Result<Self, Error> {
        Ok(super::InferenceConfig::read_user(home)?
            .unwrap_or_default()
            .updates)
    }
}

#[cfg(test)]
mod test;
