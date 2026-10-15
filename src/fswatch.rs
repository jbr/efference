//! The core fs watcher (docs/web-bind.md "Watching for changes"): one notify
//! debouncer, many consumers over a broadcast channel.
//!
//! Consumers subscribe to the whole event feed and filter to the paths they
//! care about; watches are registered per directory and refcounted, so a watch
//! lives exactly as long as something holds its [`WatchGuard`]. Events are
//! **pokes, never content** — a path, nothing else — so a lagged receiver
//! loses nothing it cannot recover by re-reading, and the channel can stay
//! shallow and drop-happy.
//!
//! Directories, not files: notify cannot watch a nonexistent path, and editors
//! save via rename-replace, which orphans an inode-level watch — the same
//! reasoning as the config watcher (`daemon/watch.rs`). Watching the parent
//! also keeps a subscription alive across its file being deleted and restored,
//! which is a case the file viewer meets in practice.
//!
//! The debouncer runs notify on its own thread and invokes the handler there;
//! `broadcast::Sender::send` is thread-safe, so events cross into async land
//! with no forwarding task. The debouncer is owned here and stops when the
//! [`FsWatch`] drops.

use notify_debouncer_full::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

/// One save fires several raw events; this is the window that coalesces them.
/// Well under human reading latency, well over an editor's write-then-rename.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// A coalesced change at a path — a poke, never content. Consumers respond by
/// re-reading whatever they derive from the path; nothing here is load-bearing
/// to miss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsChange {
    /// Absolute, as notify reports it against the watched directory.
    pub path: PathBuf,
}

/// The watcher. One per consumer-facing surface (the `/files` handler holds
/// one); future consumers subscribe to the same instance rather than spawning
/// their own.
pub struct FsWatch {
    tx: broadcast::Sender<FsChange>,
    state: Mutex<State>,
}

struct State {
    debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
    /// How many guards hold each watched directory.
    watched: HashMap<PathBuf, usize>,
}

impl std::fmt::Debug for FsWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsWatch").finish_non_exhaustive()
    }
}

impl FsWatch {
    pub fn new() -> notify_debouncer_full::notify::Result<Arc<Self>> {
        let (tx, _) = broadcast::channel(256);
        let sender = tx.clone();
        let debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                Err(errors) => {
                    for e in errors {
                        log::warn!("fswatch error: {e}");
                    }
                    return;
                }
            };
            // One poke per distinct path per batch: the debouncer already
            // coalesced in time, this coalesces across a batch's event kinds.
            let paths: BTreeSet<&PathBuf> =
                events.iter().flat_map(|ev| ev.event.paths.iter()).collect();
            for path in paths {
                // No receivers is fine — watches can outlive subscribers
                // between a disconnect and a reconnect.
                let _ = sender.send(FsChange { path: path.clone() });
            }
        })?;
        Ok(Arc::new(Self {
            tx,
            state: Mutex::new(State {
                debouncer,
                watched: HashMap::new(),
            }),
        }))
    }

    /// The whole feed — every change under every watched directory. Filter to
    /// what you care about; treat `Lagged` as "re-read and continue".
    pub fn subscribe(&self) -> broadcast::Receiver<FsChange> {
        self.tx.subscribe()
    }

    /// Test probe: how many directories are currently watched — lets a
    /// transport-level test pin that a closed stream releases its watch.
    #[cfg(test)]
    pub fn watched_count(&self) -> usize {
        self.state.lock().expect("fswatch lock").watched.len()
    }

    /// Watch a directory (non-recursive), refcounted: the guard keeps it
    /// watched, and the last guard's drop unwatches. `dir` should be canonical
    /// — events compare against it byte-wise, and macOS reports `/private/var`
    /// for a `/var` registration's events.
    pub fn watch_dir(
        self: &Arc<Self>,
        dir: &Path,
    ) -> notify_debouncer_full::notify::Result<WatchGuard> {
        let mut state = self.state.lock().expect("fswatch lock");
        match state.watched.get_mut(dir) {
            Some(count) => *count += 1,
            None => {
                state.debouncer.watch(dir, RecursiveMode::NonRecursive)?;
                state.watched.insert(dir.to_path_buf(), 1);
            }
        }
        Ok(WatchGuard {
            watch: Arc::clone(self),
            dir: dir.to_path_buf(),
        })
    }
}

/// Keeps a directory watched; the last guard for a directory unwatches it on
/// drop. Held by whatever owns the subscription — for an SSE stream, the
/// stream's own state, so the watch's lifetime is the connection's.
#[derive(Debug)]
pub struct WatchGuard {
    watch: Arc<FsWatch>,
    dir: PathBuf,
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        let mut state = self.watch.state.lock().expect("fswatch lock");
        if let Some(count) = state.watched.get_mut(&self.dir) {
            *count -= 1;
            if *count == 0 {
                state.watched.remove(&self.dir);
                let _ = state.debouncer.unwatch(&self.dir);
            }
        }
    }
}

#[cfg(test)]
mod test;
