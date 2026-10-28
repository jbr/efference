//! The newest release the host's update check found, when it is newer than the host
//! (docs/update-check.md), and the `newer_version` topic every hosted session publishes
//! it on.
//!
//! Host state: one [`NewerVersion`] lives on `Host` and is handed to each session the
//! host adopts ([`Session::set_newer_version`]), the way the attention table is. It is
//! carried as a session topic because a session's `/events` stream is what both clients
//! already follow; neither follows anything host-wide that a status line could read
//! (docs/control-api.md). A session outside a host has none and publishes `null`.

use super::*;
use crate::update_check::{INSTALL_COMMAND, Release};

/// What the host's update check last found newer than the host, or nothing.
#[derive(Default, Debug)]
pub(crate) struct NewerVersion(Mutex<Option<Release>>);

impl NewerVersion {
    /// The release, while one newer than the host is known.
    pub(crate) fn get(&self) -> Option<Release> {
        self.0.lock().unwrap().clone()
    }

    /// Record what a check found. Answers whether the value moved, so the caller
    /// wakes the sessions' topic watchers only when there is something to publish.
    pub(crate) fn set(&self, release: Option<Release>) -> bool {
        let mut held = self.0.lock().unwrap();
        let moved = *held != release;
        *held = release;
        moved
    }
}

impl Session {
    /// Hand the session the host's [`NewerVersion`]. Called once, by the host's adopt.
    pub(crate) fn set_newer_version(&self, newer: Arc<NewerVersion>) {
        let _ = self.newer_version.set(newer);
    }

    /// The `newer_version` topic: the release and the command that installs it, or
    /// `null` while none newer than the host is known.
    pub(super) fn newer_version_json(&self) -> Value {
        match self.newer_version.get().and_then(|newer| newer.get()) {
            Some(release) => serde_json::json!({
                "version": release.version,
                "tag": release.tag,
                "url": release.url,
                "update_command": INSTALL_COMMAND,
            }),
            None => Value::Null,
        }
    }
}
