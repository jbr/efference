//! A live config file: refresh, revert, and write-back (docs/permissions.md).

use super::*;

/// The live config file: the format-preserving TOML document on disk, plus the
/// [`Config`] it validates to (docs/permissions.md).
///
/// ## The invariant
///
/// **The held document is the last content that parsed, and the disk always holds it.** The file
/// is the session's policy interface — the operator reads it to see the perimeter the session ran
/// under, and copies grants up to the origin template from it — so it must never sit in a state
/// that doesn't parse. A config edit therefore has exactly two outcomes, never a silent third
/// ("approved but didn't take"): **adopted**, or **rejected and reverted with the reason**.
///
/// Two directions maintain that invariant, and reverting is not a separate mechanism — it is the
/// same [`sync`] the harness's own edits use:
///
/// - [`apply`] + [`sync`] (memory → disk): mutate the document at a key, write it out.
/// - [`refresh`] (disk → memory): re-read. On a parse, **replace** the held document wholesale (a
///   merge would resurrect keys the author deleted). On a failure, [`sync`] the last-good document
///   back over the file.
///
/// Holding [`DocumentMut`] rather than the file's text is what makes [`apply`] format-preserving
/// without a re-read, and what makes an unmodified document round-trip to its original bytes —
/// every comment, key order, and blank line survives both an edit and a revert. That matters
/// concretely: the parse-error message tells the model to consult the schema header comment, so a
/// revert that dropped comments would erase its own advice.
///
/// [`apply`]: ConfigFile::apply
/// [`sync`]: ConfigFile::sync
/// [`refresh`]: ConfigFile::refresh
#[derive(Debug)]
pub struct ConfigFile {
    path: PathBuf,
    /// The last content that parsed, as a format-preserving AST.
    doc: DocumentMut,
    /// What [`Self::doc`] validates to. Kept alongside so a reader needn't re-parse.
    config: Config,
}

/// The outcome of a [`ConfigFile::refresh`] — which of the two permitted outcomes the edit got.
///
/// `Adopted` is an order of magnitude wider than the other two variants, which
/// `large_enum_variant` flags. Boxing it would buy nothing here: a `Refreshed` is
/// returned once per refresh — an operator or an agent editing the file — and
/// destructured by the caller that receives it, so it never sits in a collection
/// and is never moved in bulk. The indirection would cost an allocation on the
/// common path and a deref at every call site.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Refreshed {
    /// The file parsed; the held document now matches disk. Carries the newly-effective config
    /// for the caller to diff against the policy in force.
    Adopted(Config),
    /// The file did not parse; the last-good document was written back over it, so disk is valid
    /// again and the effective policy is unchanged. `error` is the parse failure, verbatim, to be
    /// delivered to whoever authored the edit.
    Reverted { error: String },
    /// The file did not parse **and** the write-back failed — the one state where disk is left
    /// invalid. Reported honestly rather than swallowed: the caller cannot claim a revert.
    RevertFailed { error: String, io: std::io::Error },
}

impl ConfigFile {
    /// Read and parse `path` into a live document. Fails if it doesn't parse — a session never
    /// starts on an invalid config (boot already fails closed), which is what guarantees the held
    /// document exists for every later revert.
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let text =
            std::fs::read_to_string(path).map_err(|e| LoadError::Io(path.to_path_buf(), e))?;
        let config = parse(path, &text)?;
        let doc = text
            .parse::<DocumentMut>()
            .map_err(|e| LoadError::Parse(path.to_path_buf(), toml_edit::de::Error::from(e)))?;
        Ok(Self {
            path: path.to_path_buf(),
            doc,
            config,
        })
    }

    /// The config the held document validates to — the file's contribution to policy.
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write the held document to disk. This is both the persist path and the revert path: since
    /// the document is by construction the last content that parsed, writing it out is how the
    /// file returns to validity, whatever put it in an invalid state.
    pub fn sync(&self) -> std::io::Result<()> {
        write_atomically(&self.path, &self.doc.to_string())
    }

    /// Apply one committed [`ConfigChange`] to the held document and write it out
    /// (docs/permissions.md). **Replaces at a key** while preserving every comment
    /// and unrelated line — the property that makes "yes, always" auto-persist sound for *every*
    /// slot, including a single `scope` narrowing. Idempotent by construction: setting a
    /// key/array-member that already holds the value is a no-op on disk.
    pub fn apply(&mut self, change: &ConfigChange) -> std::io::Result<()> {
        self.apply_all(std::slice::from_ref(change))
    }

    /// Apply several committed changes to the held document and write it out once
    /// (docs/permissions.md). A batch that arrives together — a message's admission grants — is
    /// one write and one document, not one per change.
    ///
    /// The mutated document is re-validated before it is written, so the invariant ("the held
    /// document parses") cannot be broken by the harness's own edit; a change that somehow
    /// wouldn't parse is rejected with the document untouched rather than written out. The held
    /// document and its [`Self::config`] are replaced only after the write succeeds, so a failed
    /// write leaves both the file's bytes and the state this struct reports unchanged.
    pub fn apply_all(&mut self, changes: &[ConfigChange]) -> std::io::Result<()> {
        let invalid = |e: String| std::io::Error::new(std::io::ErrorKind::InvalidData, e);
        let mut doc = self.doc.clone();
        for change in changes {
            apply_to_document(&mut doc, change);
        }
        let text = config_schemas::keep_directives_first(&self.doc.to_string(), doc.to_string());
        let config = parse(&self.path, &text).map_err(|e| invalid(e.to_string()))?;
        let doc = text
            .parse::<DocumentMut>()
            .map_err(|e: toml_edit::TomlError| invalid(e.to_string()))?;
        write_atomically(&self.path, &text)?;
        self.doc = doc;
        self.config = config;
        Ok(())
    }

    /// Re-read the file: **adopt** it if it parses, **revert** it if it doesn't
    /// (docs/permissions.md). The single entry point for every way the
    /// file can change — a model's gated `write`/`edit`, an operator's hand-edit, the harness's own
    /// write-back echo — because the invariant is about the file's state, not about who authored it.
    ///
    /// An unreadable file (deleted, renamed away mid-save) takes the same path as an unparseable
    /// one: the held document is written back, restoring it.
    pub fn refresh(&mut self) -> Refreshed {
        let reverted = |file: &Self, error: String| match file.sync() {
            Ok(()) => Refreshed::Reverted { error },
            Err(io) => Refreshed::RevertFailed { error, io },
        };
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) => {
                let error = LoadError::Io(self.path.clone(), e).to_string();
                return reverted(self, error);
            }
        };
        let config = match parse(&self.path, &text) {
            Ok(config) => config,
            Err(e) => return reverted(self, e.to_string()),
        };
        // Parsed as a `Config`, so it parses as a document; a failure here would mean the two
        // parsers disagree, which is a revert like any other invalid state.
        let doc = match text.parse::<DocumentMut>() {
            Ok(doc) => doc,
            Err(e) => return reverted(self, e.to_string()),
        };
        self.doc = doc;
        self.config = config.clone();
        Refreshed::Adopted(config)
    }
}
/// The counter [`write_atomically`] names its temp files with. Not state: it exists only so two
/// writes in one process never contend for one temp name, and its value carries no meaning.
static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Replace `path`'s contents with `text` so that a reader — an editor, a watcher, the next boot —
/// sees either the previous bytes or these, never a truncated file.
///
/// The text goes to a temp file beside the target, is flushed with `sync_all`, and is renamed
/// into place; a failure after the temp is created removes it and leaves the target untouched, so
/// a caller that gets an error still has the file it had before. A temp this call did not create —
/// `create_new` found the name taken — is left alone. The temp file is in the same directory as
/// the target, so the rename cannot cross a filesystem.
///
/// A `path` that is a symlink is resolved first and its **target** is replaced: renaming over the
/// link would swap the operator's symlink — a config kept in a dotfiles checkout, say — for a
/// regular file. A link whose target does not resolve is an error rather than a licence to write
/// over the link, which would destroy it the same way ([`replace_target`]). The target's
/// permission bits are preserved, which a plain create does not do (the umask would apply). A
/// hardlinked config keeps its other links on the old bytes, because the rename replaces one
/// directory entry; a config reached through a hardlink is not a shape the session supports.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let invalid = |message: &str| std::io::Error::new(std::io::ErrorKind::InvalidInput, message);
    let target = replace_target(path)?;
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = target
        .file_name()
        .ok_or_else(|| invalid("config path has no file name"))?;
    let count = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".tmp-{}-{count}", std::process::id()));
    let temp = dir.join(temp_name);

    let mode = std::fs::metadata(&target)
        .map(|m| m.permissions().mode())
        .ok();
    // Open before the cleanup is in scope: a failed `create_new` means the name belongs to
    // another file, and removing it would delete that file.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode.unwrap_or(crate::record::session_dir::FILE_MODE))
        .open(&temp)?;
    let written = (|| {
        file.write_all(text.as_bytes())?;
        if let Some(mode) = mode {
            // `open`'s mode is filtered through the umask; the target's own mode is not.
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        file.sync_all()
    })();
    let placed = written.and_then(|()| std::fs::rename(&temp, &target));
    if placed.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    placed
}

/// The path [`write_atomically`] replaces.
///
/// A symlink is followed to its target, because renaming over the link would replace the
/// operator's symlink with a regular file. A link whose target does not resolve is an error, for
/// the same reason: the link is the operator's file, and writing a regular file where it stood
/// destroys it. A leaf that does not exist yet is created where it would be, with any symlinked
/// directory on the way resolved; anything else that will not resolve is refused rather than
/// guessed at.
fn replace_target(path: &Path) -> std::io::Result<PathBuf> {
    let invalid = |message: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, message);
    match std::fs::canonicalize(path) {
        Ok(target) => Ok(target),
        Err(error) => match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(std::io::Error::new(
                error.kind(),
                format!(
                    "config path {} is a symlink whose target does not resolve: {error}",
                    path.display()
                ),
            )),
            // Present but unresolvable for another reason: a component is not searchable, or the
            // path loops. Refuse rather than write somewhere the caller did not name.
            Ok(_) => Err(error),
            Err(leaf) if leaf.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = path.file_name() else {
                    return Err(invalid(format!(
                        "config path {} has no file name",
                        path.display()
                    )));
                };
                let dir = match path.parent() {
                    Some(dir) if !dir.as_os_str().is_empty() => dir,
                    _ => Path::new("."),
                };
                let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
                Ok(dir.join(name))
            }
            Err(other) => Err(other),
        },
    }
}
