//! An island's exit (docs/resumption.md, docs/operating.md): when the launcher that
//! owns it goes, the host archives every session it holds through [`Host::end_by`], the
//! path `DELETE /sessions/:id` takes, naming [`STANDALONE_EXIT`] as the archive's
//! `source`, and only then shuts down.
//!
//! The order is what makes it an archive. `end` archives only while the host's
//! `Swansong` is running: a session it reaches once the shutdown has begun is stopped
//! without `Archived` and keeps its worktree, which is a host shutdown's behaviour. So
//! the shutdown starts once every session has left the map. A crash, a signal, and the
//! shutdown of a host that is not an island take none of this, and leave their
//! sessions interrupted.

use super::*;
use std::collections::HashSet;
use std::sync::atomic::Ordering;

/// The `source` an island's exit records on each `Archived` entry it appends, so the
/// log tells the standalone closing from an operator's archive
/// (docs/log-and-projection.md).
const STANDALONE_EXIT: &str = "standalone exit";

impl Host {
    /// Make this host an island: set by the owner watch that adopts it
    /// (`crate::daemon::island`), before the boot resume is spawned.
    pub(crate) fn become_island(&self) {
        self.island.store(true, Ordering::SeqCst);
    }

    /// Whether an owner watch has adopted this host.
    pub(crate) fn is_island(&self) -> bool {
        self.island.load(Ordering::SeqCst)
    }

    /// The island's owner has gone: archive every session on this host, then shut
    /// it down. Starting the shutdown is the whole act; the listener awaits its
    /// completion, as it does a signal's.
    pub(crate) async fn archive_and_shut_down(self: &Arc<Self>) {
        self.archive_all().await;
        drop(self.swansong.shut_down());
    }

    /// Archive every session on this host, starting or hosted, and return once none
    /// is left. A non-detached child is left to the cascade its creator's end runs
    /// (`adopt_prepared`), so each session is archived once. A session that arrives
    /// meanwhile is taken by the next round.
    async fn archive_all(self: &Arc<Self>) {
        loop {
            let ids = self.ids();
            if ids.is_empty() {
                return;
            }
            let cascaded: HashSet<String> = ids.iter().flat_map(|id| self.cascade_of(id)).collect();
            let mut roots: Vec<String> = ids
                .iter()
                .filter(|id| !cascaded.contains(*id))
                .cloned()
                .collect();
            // Only a cycle of creators leaves no root. Lineage is kept only for a
            // creator that is already hosted, so none forms; ending them all keeps
            // the loop from spinning if one ever did.
            if roots.is_empty() {
                roots = ids;
            }
            let mut ending = tokio::task::JoinSet::new();
            for id in &roots {
                if let Some(end) = self.end_by(id, Some(STANDALONE_EXIT.into())) {
                    ending.spawn(end);
                }
            }
            while ending.join_next().await.is_some() {}
            self.left_the_map(&roots).await;
        }
    }

    /// Wait until none of `ids` is in the map. A hosted session leaves once the
    /// watcher `adopt_prepared` spawned has run its cascade, so this also waits out
    /// the children that end with it.
    async fn left_the_map(&self, ids: &[String]) {
        loop {
            // Armed before the map is read, as in `listing_stream`.
            let changed = self.listing_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let present = self.ids();
            if !ids.iter().any(|id| present.contains(id)) {
                return;
            }
            changed.await;
        }
    }
}
