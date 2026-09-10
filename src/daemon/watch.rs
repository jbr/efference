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
pub(crate) async fn config_watch_loop(shared: Arc<Session>, path: PathBuf) {
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
pub(crate) fn refresh_and_emit(shared: &Session) -> Option<Refreshed> {
    let outcome = shared.refresh_config()?;
    let file_config = match outcome {
        Refreshed::Adopted(config) => config,
        Refreshed::Reverted { ref error } => {
            // The revert put valid content back on disk, so a prior park (if any) is over —
            // the file parses again, which is the whole condition (docs/permissions.md).
            shared.clear_config_repair();
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
            // back. Say so plainly rather than implying a revert that didn't happen — and park the
            // loop on it (docs/permissions.md) rather than running on indefinitely under a policy
            // the file no longer shows. Safe to make terminal because a config write is never
            // autonomous, so there is always an owner to do the repair.
            eprintln!("[config: edit did not parse AND the revert failed: {error} / {io}]");
            shared.notify(
                "config".to_string(),
                format!(
                    "Your config edit did not parse, and restoring the last valid config also \
                     failed — the file on disk is currently invalid and the effective policy is \
                     unchanged (last good config still in force). The loop is parked until the \
                     file parses again; it needs a human repair. \
                     Parse error: {error}. Restore error: {io}"
                ),
                Some("config-parse-error".to_string()),
                None,
            );
            shared.require_config_repair(crate::daemon::ConfigRepair {
                path: shared
                    .config_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                at: crate::log::Timestamp::now().0,
                message: "config unparseable — the loop is parked until a human repairs the file"
                    .to_string(),
                parse_error: error.clone(),
                restore_error: io.to_string(),
            });
            return Some(outcome);
        }
    };
    // Adopted: the file parses, so a park on it is over and the loop wakes
    // (docs/permissions.md). This is the human-repair path — they fix the file,
    // fs-watch lands here, the gate clears.
    shared.clear_config_repair();
    // A newly-adopted `[env]` table can only *add* needles (docs/env.md): the set
    // is append-only for the session, because a variable an edit stopped masking
    // may still sit in files this session goes on to read. `ConfigChange` has no
    // `env` variant, so the file itself is the only way the table moves — which is
    // why the absorb hangs here rather than off the edit fold. A reference in the
    // edited table resolves here too, at the one moment an operator is demonstrably
    // present, so a vault prompt has someone to answer it. A failure is announced
    // on its own stable slot rather than left to surface as a tool's opaque auth
    // error later; the value in force is unchanged, because injection is fixed at
    // boot.
    let unresolved = shared.absorb_env_table(&file_config);
    if !unresolved.is_empty() {
        let detail = unresolved
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        eprintln!("[config: unresolved [env] reference: {detail}]");
        shared.notify(
            "config".to_string(),
            format!(
                "An `[env]` reference in your config did not resolve, so no value was fetched \
                 for it: {detail}. The rest of the edit took effect. Values are injected into \
                 tool subprocesses at session start, so a reference added now takes effect on \
                 the next session either way."
            ),
            Some("config-env-resolve".to_string()),
            None,
        );
    }
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
