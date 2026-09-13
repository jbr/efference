//! Complete child environments and spawn policy (docs/env.md).

use super::EnvSnapshot;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The `[env]` table's effect on a child environment at spawn: names removed
/// (`strip`), then values set (an entry that names a `value`, a `cmd`, or a
/// reference). Cloned into each spawn site (`Arc`-backed), so a new site costs
/// one `apply` call rather than a re-derivation of policy.
///
/// Removals run before sets, so a name that is both stripped and injected ends
/// up with the injected value — injecting it is the point of writing a value.
#[derive(Clone)]
pub struct ChildEnv {
    base: EnvSnapshot,
    strip: Arc<Vec<String>>,
    inject: Arc<BTreeMap<String, String>>,
}

impl std::fmt::Debug for ChildEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildEnv")
            .field("base", &self.base)
            .field("strip", &self.strip)
            .field("injected_names", &self.inject.keys())
            .finish()
    }
}

impl Default for ChildEnv {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl ChildEnv {
    pub fn new(strip: Vec<String>) -> Self {
        Self::from_snapshot(
            EnvSnapshot::capture().expect("valid local environment"),
            strip,
        )
    }

    pub fn from_snapshot(base: EnvSnapshot, strip: Vec<String>) -> Self {
        Self {
            base,
            strip: Arc::new(strip),
            inject: Arc::new(BTreeMap::new()),
        }
    }

    pub fn with_inject(mut self, inject: BTreeMap<String, String>) -> Self {
        self.inject = Arc::new(inject);
        self
    }

    pub fn names(&self) -> &[String] {
        &self.strip
    }

    /// The names this spawn *sets*, without their values — what a display or a
    /// diagnostic can say out loud.
    pub fn injected_names(&self) -> impl Iterator<Item = &str> {
        self.inject.keys().map(String::as_str)
    }
    /// The environment as a child receives it: the base less the stripped names,
    /// plus the table's injected values (docs/env.md). A snapshot of the spawn
    /// policy, for handing to another session's creation request.
    pub fn resolved(&self) -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> = self
            .base
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        for name in self.strip.iter() {
            env.remove(name);
        }
        for (name, value) in self.inject.iter() {
            env.insert(name.clone(), value.clone());
        }
        env
    }

    /// Apply the policy to a `std::process::Command`'s environment.
    pub fn apply(&self, cmd: &mut std::process::Command) {
        self.base.apply(cmd);
        for name in self.strip.iter() {
            cmd.env_remove(name);
        }
        for (name, value) in self.inject.iter() {
            cmd.env(name, value);
        }
    }

    /// The `tokio::process::Command` half of [`apply`](Self::apply).
    pub fn apply_tokio(&self, cmd: &mut tokio::process::Command) {
        self.apply(cmd.as_std_mut());
    }
}
