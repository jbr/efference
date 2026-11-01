//! The host's update check (docs/update-check.md): it asks at start and then about once
//! a day, and holds what it found for every session's `newer_version` topic. The launch
//! starts it only when the user configuration answered `[updates] check = true`. It
//! never installs anything.

use crate::session::Session;
use std::time::SystemTime;

use super::*;
use crate::platform::update_check::{Release, UpdateCheck};

/// How long after a check that got an answer the next one runs.
const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long after a failed check the next one runs, so a host started offline asks
/// again within the hour rather than the next day.
const RETRY: Duration = Duration::from_secs(60 * 60);

/// How often the task reads the clock. The wait is measured on the wall clock: a
/// monotonic sleep stops counting while the machine sleeps, so a day's wait would
/// stretch by every night a laptop spends closed.
const TICK: Duration = Duration::from_secs(5 * 60);

impl Host {
    /// Ask `check` now, and again each time [`INTERVAL`] has passed ([`RETRY`] after a
    /// failure), under this host's `Swansong`. A failure is logged and changes nothing
    /// a client reads: the last answer stands.
    pub fn check_for_updates(self: &Arc<Self>, check: UpdateCheck) {
        let host = Arc::downgrade(self);
        tokio::spawn(self.swansong.interrupt(async move {
            let mut due = SystemTime::now();
            let mut failing = false;
            loop {
                if SystemTime::now() >= due {
                    let outcome = check.newer().await;
                    let Some(host) = host.upgrade() else {
                        return;
                    };
                    let wait = match outcome {
                        Ok(newer) => {
                            failing = false;
                            host.record_newer_version(newer);
                            INTERVAL
                        }
                        // Said once per run of failures: an offline machine would
                        // otherwise repeat it every hour.
                        Err(why) if failing => {
                            log::debug!("update check failed again: {why}");
                            RETRY
                        }
                        Err(why) => {
                            failing = true;
                            log::warn!("update check failed: {why}");
                            RETRY
                        }
                    };
                    due = SystemTime::now() + wait;
                }
                tokio::time::sleep(TICK).await;
            }
        }));
    }

    /// Hold `newer`, the release a check found newer than this host or `None`, and
    /// wake every hosted session's topic watcher when it moved.
    pub(crate) fn record_newer_version(&self, newer: Option<Release>) {
        if let Some(release) = &newer
            && self.newer_version.get().as_ref() != Some(release)
        {
            log::info!(
                "efference {} is available; this host is {}",
                release.version,
                crate::platform::update_check::INSTALLED
            );
        }
        if !self.newer_version.set(newer) {
            return;
        }
        let sessions: Vec<Arc<Session>> = self
            .sessions
            .lock()
            .unwrap()
            .hosted_iter()
            .map(|(_, hosted)| Arc::clone(&hosted.session))
            .collect();
        for session in sessions {
            session.touch_state();
        }
    }
}
