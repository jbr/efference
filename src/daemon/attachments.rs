//! The attachment watcher — a file appearing in the store becomes a notification
//! (docs/inbox.md).
//!
//! One rule: **a file that appears in the top level of `.efference/<id>/attachments/` is an
//! attachment**, whatever put it there — a file-manager drag, a `mv`, a `curl -o`. It is the same
//! affordance as editing `config.toml` for immediate effect, on the same machinery
//! (`daemon/watch.rs`). A composer's uploads land in `composer/` instead and are announced by the
//! turn that carries them; the watcher never reads that subdirectory.
//!
//! Nothing here touches the log directly. The notification rides the existing inbox rail
//! (`Session::notify`), is shown at a triage, and is logged as `Ambient` like any other
//! (at first show, since its slot is the file's path) — so the bytes stay on disk until
//! the model pulls them, and the byte-stability pins never see the attachment at all
//! (docs/log-and-projection.md).

use super::*;
use std::path::Path;

impl Session {
    /// Record the session's attachment store. Called once by the runtime.
    pub(crate) fn set_attachment_store(&self, store: crate::attachments::Store) {
        let _ = self.attachment_store.set(store);
    }

    /// Where a composer's uploads go (docs/inbox.md): the store's `composer/`
    /// subdirectory, which the watcher does not read, so an upload is described
    /// once — by the turn that carries it. `None` when the runtime set no store.
    pub(crate) fn composer_store(&self) -> Option<crate::attachments::Store> {
        self.attachment_store
            .get()
            .map(crate::attachments::Store::composer)
    }
}

/// How long arrivals are coalesced before the watcher looks. A drag of several files, or an
/// editor's write-then-rename, fires a burst; one look per burst is one notice per file rather
/// than several per file.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// Watch the attachment store and enqueue a notification per newly-arrived file.
///
/// `display_dir` is how the store is addressed in the notice — the path the model will hand to
/// `read` / `read_image` — while `dir` is where the watcher looks. They differ whenever the
/// daemon was started from somewhere other than the session's parent.
///
/// The watcher owns its debouncer, so watching lives exactly as long as this task. Files already
/// present at boot are recorded as seen without notifying: a restarted daemon re-announcing every
/// attachment the previous session already handled would be noise, and the files are still there
/// to read.
pub(crate) async fn attachments_watch_loop(
    shared: Arc<Session>,
    dir: PathBuf,
    display_dir: PathBuf,
) {
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("attachments: could not create {}: {e}", dir.display());
        return;
    }
    let mut seen: std::collections::HashSet<PathBuf> = read_names(&dir);

    let (tx, mut rx) = mpsc::unbounded_channel::<DebounceEventResult>();
    let mut debouncer = match new_debouncer(DEBOUNCE, None, move |res| {
        let _ = tx.send(res);
    }) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("attachments: could not start watcher: {e}");
            return;
        }
    };
    if let Err(e) = debouncer.watch(&dir, RecursiveMode::NonRecursive) {
        log::warn!("attachments: could not watch {}: {e}", dir.display());
        return;
    }

    while rx.recv().await.is_some() {
        // The event batch says *something* changed; the directory says what is there now. Reading
        // it is cheaper to reason about than reconstructing state from create/rename/remove
        // events, and it is self-correcting after a missed or coalesced event.
        let current = read_names(&dir);
        let mut arrived: Vec<PathBuf> = current.difference(&seen).cloned().collect();
        arrived.sort();
        for path in arrived {
            announce(&shared, &display_dir, &path);
        }
        seen = current;
    }
}

/// The store's current regular files, by path. Directories and dotfiles are skipped: a dropped
/// folder is not a single attachment, and a `.DS_Store` is not one at all. Skipping directories is
/// also what keeps `composer/` out: an upload is announced by the turn that carries it, and
/// describing it here too would say it twice.
fn read_names(dir: &Path) -> std::collections::HashSet<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Default::default();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| {
            !e.file_name().to_string_lossy().starts_with('.')
                && e.file_type().is_ok_and(|t| t.is_file())
        })
        .map(|e| e.path())
        .collect()
}

/// Describe one arrival and put it on the inbox rail. A file that cannot be statted is skipped
/// silently — a half-written file is the common case, and the next event describes it whole.
fn announce(shared: &Session, display_dir: &Path, path: &Path) {
    let Some(name) = path.file_name() else { return };
    let display = display_dir.join(name).to_string_lossy().into_owned();
    let Ok(arrival) = crate::attachments::inspect(&display, path) else {
        return;
    };
    let (source, body, slot) = crate::attachments::notification(&arrival);
    shared.notify(source, body, slot);
}
