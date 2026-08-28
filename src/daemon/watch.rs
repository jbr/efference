//! The config-file watcher: external edits become committed `ConfigEdit`s (docs/permissions.md).

use super::*;

/// The fs-watch initiator (docs/permissions.md): watch the config file and
/// turn external edits into **committed** `ConfigEdit`s. A human editing the file *is* the
/// orchestrator committing, so each diff lands directly as `Committed` (not a proposal) and folds
/// into the effective policy live — no restart.
///
/// We watch the **parent directory** and filter to the filename, not the file inode: editors
/// save-via-rename (write a temp, rename over the original), which orphans an inode-level watch.
/// Events are debounced (one save fires several raw events). The harness's own auto-persist
/// write-back echoes back through here harmlessly — [`config::diff_changes`] is additive and the
/// file already matches the effective config, so the echo diffs to nothing.
///
/// The debouncer runs `notify` on its own thread and invokes the handler there; we forward each
/// batch into this async task over a channel, and the task **owns** the debouncer so watching lives
/// exactly as long as the task (the `Debouncer` guard stops on drop).
pub(crate) async fn config_watch_loop(shared: Arc<Shared>, path: PathBuf) {
    // A relative `efference.toml` has an empty parent; watch cwd in that case.
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let target = path.file_name().map(|n| n.to_os_string());

    let (tx, mut rx) = mpsc::unbounded_channel::<DebounceEventResult>();
    let mut debouncer = match new_debouncer(Duration::from_millis(400), None, move |res| {
        // The handler runs on notify's thread — just hand the batch to the async side.
        let _ = tx.send(res);
    }) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "[config-watch: could not start watcher for {}: {e}]",
                path.display()
            );
            return;
        }
    };
    if let Err(e) = debouncer.watch(&dir, RecursiveMode::NonRecursive) {
        eprintln!("[config-watch: could not watch {}: {e}]", dir.display());
        return;
    }
    eprintln!("watching config: {}", path.display());

    while let Some(result) = rx.recv().await {
        let events = match result {
            Ok(events) => events,
            Err(errors) => {
                for e in errors {
                    eprintln!("[config-watch error: {e}]");
                }
                continue;
            }
        };
        // We watch the whole dir; react only to events that touch our file. Compare by file name —
        // robust to relative/absolute path differences in how the backend reports the event path.
        let touches = events.iter().any(|ev| {
            ev.event
                .paths
                .iter()
                .any(|p| p.file_name().map(|n| n.to_os_string()) == target)
        });
        if touches {
            refresh_and_emit(&shared);
        }
    }
}

/// Refresh the config file and emit a `Committed` `ConfigEdit` per additive/replace delta against
/// the current effective policy (docs/permissions.md). Diffing against
/// *effective* means only genuinely-new deltas surface, so the harness's own write-back is a no-op
/// echo.
///
/// A file that doesn't parse is **reverted**, not merely skipped
/// (docs/permissions.md): the disk always holds the last content that
/// parsed, so an edit is either adopted or undone-with-a-reason — never approved-but-inert. The
/// revert is announced on a stable slot, which supersedes, so repeated bad saves collapse to one
/// live notice rather than piling up.
///
/// Returns the outcome so the loop — which calls this synchronously after an approved config write
/// — can put a rejection in that call's tool result, where the author will actually see it.
pub(crate) fn refresh_and_emit(shared: &Shared) -> Option<Refreshed> {
    let outcome = shared.refresh_config()?;
    let file_config = match outcome {
        Refreshed::Adopted(config) => config,
        Refreshed::Reverted { ref error } => {
            eprintln!("[config: edit did not parse, reverted to last good: {error}]");
            shared.notify(
                "config".to_string(),
                format!(
                    "Your config edit did not take effect — it did not parse, so the file has been \
                     restored to the last valid config and the effective policy is unchanged. \
                     Re-edit it so it parses (check that `scope` is a string, not a `[scope]` \
                     table, and that keys match the schema in the file's header comment). \
                     Error: {error}"
                ),
                Some("config-parse-error".to_string()),
                None,
            );
            return Some(outcome);
        }
        Refreshed::RevertFailed { ref error, ref io } => {
            // The one state the invariant can't reach: the file is invalid and we couldn't put it
            // back. Say so plainly rather than implying a revert that didn't happen.
            eprintln!("[config: edit did not parse AND the revert failed: {error} / {io}]");
            shared.notify(
                "config".to_string(),
                format!(
                    "Your config edit did not parse, and restoring the last valid config also \
                     failed — the file on disk is currently invalid and the effective policy is \
                     unchanged (last good config still in force). It needs a human repair. \
                     Parse error: {error}. Restore error: {io}"
                ),
                Some("config-parse-error".to_string()),
                None,
            );
            return Some(outcome);
        }
    };
    // A newly-adopted `[env]` table can only *add* needles (docs/env.md): the set
    // is append-only for the session, because a variable an edit stopped masking
    // may still sit in files this session goes on to read. `ConfigChange` has no
    // `env` variant, so the file itself is the only way the table moves — which is
    // why the absorb hangs here rather than off the edit fold.
    shared.absorb_env_table(&file_config);
    let changes = crate::config::diff_changes(&shared.effective_config(), &file_config);
    if changes.is_empty() {
        return Some(Refreshed::Adopted(file_config));
    }
    let turn = shared.latest_turn();
    for change in changes {
        // The file edit *is* the commit, and it lives in the file by definition → `Persist` (the
        // audit answer to "survives a reboot?" is yes). It does not go through `commit_config_edit`,
        // so it is *not* written back — that would loop against the very write that triggered us.
        let edit = crate::log::ConfigEdit {
            state: EditState::Committed,
            resolves: None,
            change,
            note: "config file edited".into(),
        };
        if let Err(e) = shared.append(turn, Payload::ConfigEdit(edit)) {
            eprintln!("[config-watch: append failed: {e}]");
        }
    }
    Some(Refreshed::Adopted(file_config))
}
