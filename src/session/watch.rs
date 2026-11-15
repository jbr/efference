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
/// write-back echoes back through here harmlessly —
/// [`config::diff_changes`](crate::config::diff_changes) is additive and the
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
    let target = path.file_name().map(std::ffi::OsStr::to_os_string);

    let (tx, mut rx) = mpsc::unbounded_channel::<DebounceEventResult>();
    let mut debouncer = match new_debouncer(Duration::from_millis(400), None, move |res| {
        // The handler runs on notify's thread — just hand the batch to the async side.
        let _ = tx.send(res);
    }) {
        Ok(d) => d,
        Err(e) => {
            shared.warn(format_args!(
                "config-watch: could not start watcher for {}: {e}",
                path.display()
            ));
            return;
        }
    };
    if let Err(e) = debouncer.watch(&dir, RecursiveMode::NonRecursive) {
        shared.warn(format_args!(
            "config-watch: could not watch {}: {e}",
            dir.display()
        ));
        return;
    }
    log::debug!("watching config: {}", path.display());

    while let Some(result) = rx.recv().await {
        let events = match result {
            Ok(events) => events,
            Err(errors) => {
                for e in errors {
                    shared.warn(format_args!("config-watch error: {e}"));
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
                .any(|p| p.file_name().map(std::ffi::OsStr::to_os_string) == target)
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
    let pass = {
        let _section = shared.policy_section();
        refresh_locked(shared)
    };
    raise_pass(shared, pass)
}

/// One pass over the config file, taken with the policy section held (docs/permissions.md).
///
/// Everything that raises a notification is deferred into this value — `notify` re-enters
/// `append` for a slot-less notification, and a caller of this is already inside the section —
/// and the `[env]` absorb stays out of the section entirely, because resolving a reference can
/// block on an operator. Admission calls this under its own section, so it must not take one.
pub(crate) struct Pass {
    /// What the file is now.
    file: FileState,
    /// The parse-error notices the pass owes, raised after the section is released.
    notices: Vec<Notification>,
    /// The park an unrecoverable invalid file leaves behind, set after the section.
    repair: Option<ConfigRepair>,
}

/// The state one [`refresh_locked`] pass leaves the file in.
enum FileState {
    /// The session has no config file (the built-in default role).
    NoFile,
    /// The file parsed and is adopted, with the notices its deltas earned.
    // Boxed: this is the one large variant, and a `Pass` is carried by value through
    // `refresh_locked`'s return — the config is moved, not cloned, so the indirection costs
    // nothing the pass would not cost anyway.
    Adopted {
        config: Box<crate::config::Config>,
        notices: Vec<(String, String)>,
    },
    /// The file did not parse and the last-good document was written back over it.
    Reverted { error: String },
    /// The file did not parse and could not be restored.
    RevertFailed { error: String, io: io::Error },
}

impl Pass {
    /// Whether a grant may be computed and written after this pass: an invalid file that could
    /// not be restored is the one state where the file cannot be persisted to, so an admission
    /// on it admits its message and refuses its grants (docs/permissions.md).
    pub(crate) fn grants_usable(&self) -> bool {
        !matches!(self.file, FileState::RevertFailed { .. })
    }
}

/// Refresh the file, adopt or revert it, and reconcile what it holds against the policy in
/// force — all with the caller's policy section held. The returned [`Pass`] carries what still
/// has to be said or done outside it.
pub(crate) fn refresh_locked(shared: &Session) -> Pass {
    let Some(outcome) = shared.refresh_config() else {
        return Pass {
            file: FileState::NoFile,
            notices: Vec::new(),
            repair: None,
        };
    };
    match outcome {
        Refreshed::Adopted(file_config) => {
            let changes = crate::config::diff_changes(&shared.effective_config(), &file_config);
            let notices = if changes.is_empty() {
                Vec::new()
            } else {
                let turn = shared.latest_turn();
                let edits = changes
                    .into_iter()
                    .map(|change| {
                        // The file edit *is* the commit, and it lives in the file by definition →
                        // `Persist` (the audit answer to "survives a reboot?" is yes). It does not
                        // go through `commit_config_edit`, so it is *not* written back — that would
                        // loop against the very write that triggered us.
                        crate::record::log::ConfigEdit::new(EditState::Committed, change)
                            .with_note("config file edited".into())
                    })
                    .collect::<Vec<_>>();
                let batch = shared.append_committed_locked(turn, &edits);
                if let Some(e) = &batch.error {
                    // Partly logged, not all of it: the edits before the failure are in the
                    // effective policy and in the log, and the rest stay in the file until a
                    // later pass diffs them in again (docs/permissions.md).
                    shared.warn(format_args!(
                        "config-watch: {} of {} config edits appended: {e}",
                        batch.landed,
                        edits.len()
                    ));
                }
                batch.notices
            };
            Pass {
                file: FileState::Adopted {
                    config: Box::new(file_config),
                    notices,
                },
                notices: Vec::new(),
                repair: None,
            }
        }
        Refreshed::Reverted { error } => {
            log::debug!("config: edit did not parse, reverted to last good: {error}");
            Pass {
                notices: vec![Notification::new(
                    "config",
                    format!(
                        "Your config edit did not take effect — it did not parse, so the file has \
                         been restored to the last valid config and the effective policy is \
                         unchanged. Re-edit it so it parses (check that `scope` is a string, not a \
                         `[scope]` table, and that keys match the schema in the file's header \
                         comment). Error: {error}"
                    ),
                    Some("config-parse-error".to_string()),
                )],
                file: FileState::Reverted { error },
                repair: None,
            }
        }
        Refreshed::RevertFailed { error, io } => {
            // The one state the invariant can't reach: the file is invalid and we couldn't put it
            // back. Say so plainly rather than implying a revert that didn't happen — and park the
            // loop on it (docs/permissions.md) rather than running on indefinitely under a policy
            // the file no longer shows. Safe to make terminal because a config write is never
            // autonomous, so there is always an owner to do the repair.
            shared.warn(format_args!(
                "config: edit did not parse AND the revert failed: {error} / {io}"
            ));
            Pass {
                notices: vec![Notification::new(
                    "config",
                    format!(
                        "Your config edit did not parse, and restoring the last valid config also \
                         failed — the file on disk is currently invalid and the effective policy \
                         is unchanged (last good config still in force). The loop is parked until \
                         the file parses again; it needs a human repair. \
                         Parse error: {error}. Restore error: {io}"
                    ),
                    Some("config-parse-error".to_string()),
                )],
                repair: Some(ConfigRepair {
                    path: shared
                        .config_path()
                        .map_or_default(|p| p.display().to_string()),
                    at: crate::record::log::Timestamp::now().0,
                    message: "config unparseable — the loop is parked until a human repairs the \
                              file"
                        .to_string(),
                    parse_error: error.clone(),
                    restore_error: io.to_string(),
                }),
                file: FileState::RevertFailed { error, io },
            }
        }
    }
}

/// Raise what a [`Pass`] owes and report its outcome, with the policy section released
/// (docs/permissions.md).
pub(crate) fn raise_pass(shared: &Session, pass: Pass) -> Option<Refreshed> {
    match pass.file {
        FileState::NoFile => None,
        FileState::Reverted { error } => {
            // The revert put valid content back on disk, so a prior park (if any) is over —
            // the file parses again, which is the whole condition (docs/permissions.md).
            shared.clear_config_repair();
            for notice in pass.notices {
                shared.notify(notice);
            }
            Some(Refreshed::Reverted { error })
        }
        FileState::RevertFailed { error, io } => {
            for notice in pass.notices {
                shared.notify(notice);
            }
            if let Some(repair) = pass.repair {
                shared.require_config_repair(repair);
            }
            Some(Refreshed::RevertFailed { error, io })
        }
        FileState::Adopted { config, notices } => {
            // Adopted: the file parses, so a park on it is over and the loop wakes
            // (docs/permissions.md). This is the human-repair path — they fix the file,
            // fs-watch lands here, the gate clears.
            shared.clear_config_repair();
            for notice in pass.notices {
                shared.notify(notice);
            }
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
            let unresolved = shared.absorb_env_table(&config);
            if !unresolved.is_empty() {
                let detail = unresolved
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                log::debug!("config: unresolved [env] reference: {detail}");
                shared.notify(Notification::new(
                    "config",
                    format!(
                        "An `[env]` reference in your config did not resolve, so no value was \
                         fetched for it: {detail}. The rest of the edit took effect. Values are \
                         injected into tool subprocesses at session start, so a reference added \
                         now takes effect on the next session either way."
                    ),
                    Some("config-env-resolve".to_string()),
                ));
            }
            shared.raise_config_notices(notices);
            Some(Refreshed::Adopted(*config))
        }
    }
}
