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

    pub fn new(values: BTreeMap<String, String>) -> Result<Self, String> {
        for (name, value) in &values {
            if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
                return Err("environment contains an invalid name or NUL byte".into());
            }
        }
        Ok(Self(Arc::new(values)))
    }

    /// The entries, for the one place a snapshot leaves the process: the body of
    /// a session-creation request (docs/control-api.md). Consuming, so the map is
    /// handed to a request builder rather than kept around; the type itself stays
    /// without Debug or Serialize.
    pub fn into_entries(self) -> BTreeMap<String, String> {
        Arc::try_unwrap(self.0).unwrap_or_else(|shared| (*shared).clone())
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

    /// [`apply`](Self::apply) for a command spawned on the async runtime, which
    /// is how a launch environment reaches its hook.
    pub fn apply_tokio(&self, command: &mut tokio::process::Command) {
        command.env_clear().envs(self.0.iter());
    }
}

#[cfg(test)]
mod test;
