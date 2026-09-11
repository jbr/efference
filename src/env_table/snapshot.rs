//! The caller's session environment, held only in memory (docs/env.md).

use std::collections::BTreeMap;
use std::sync::Arc;

/// A complete environment, not an overlay on the hosting process. Values are
/// omitted from Debug and this type has no serialization implementation.
#[derive(Clone, Default)]
pub struct EnvSnapshot(Arc<BTreeMap<String, String>>);

impl std::fmt::Debug for EnvSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvSnapshot")
            .field("entries", &self.0.len())
            .finish()
    }
}

impl EnvSnapshot {
    /// Capture at the local creation boundary. Reject non-UTF-8 rather than
    /// silently changing or dropping a value the caller supplied.
    pub fn capture() -> Result<Self, String> {
        let mut values = BTreeMap::new();
        for (name, value) in std::env::vars_os() {
            let name = name
                .into_string()
                .map_err(|_| "environment contains a non-UTF-8 name")?;
            let value = value
                .into_string()
                .map_err(|_| format!("environment variable {name:?} has a non-UTF-8 value"))?;
            values.insert(name, value);
        }
        Self::new(values)
    }

    pub fn new(mut values: BTreeMap<String, String>) -> Result<Self, String> {
        for (name, value) in &values {
            if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
                return Err("environment contains an invalid name or NUL byte".into());
            }
        }
        // Listener credentials are not session inputs, even when the local
        // caller also launched the listener (docs/web-bind.md).
        values.remove(crate::auth::ROOT_ENV);
        Ok(Self(Arc::new(values)))
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.0.iter()
    }

    /// Replace inheritance before applying policy and harness-owned values.
    pub fn apply(&self, command: &mut std::process::Command) {
        command.env_clear().envs(self.0.iter());
    }
}

#[cfg(test)]
mod test;
