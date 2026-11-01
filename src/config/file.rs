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
        std::fs::write(&self.path, self.doc.to_string())
    }

    /// Apply a committed [`ConfigChange`] to the held document and write it out
    /// (docs/permissions.md). **Replaces at a key** while preserving every comment
    /// and unrelated line — the property that makes "yes, always" auto-persist sound for *every*
    /// slot, including a single `scope` narrowing. Idempotent by construction: setting a
    /// key/array-member that already holds the value is a no-op on disk.
    ///
    /// The mutated document is re-validated before it is written, so the invariant ("the held
    /// document parses") cannot be broken by the harness's own edit; a change that somehow
    /// wouldn't parse is rejected with the document untouched rather than written out.
    pub fn apply(&mut self, change: &ConfigChange) -> std::io::Result<()> {
        let invalid = |e: String| std::io::Error::new(std::io::ErrorKind::InvalidData, e);
        let mut doc = self.doc.clone();
        apply_to_document(&mut doc, change);
        let text = config_schemas::keep_directives_first(&self.doc.to_string(), doc.to_string());
        let config = parse(&self.path, &text).map_err(|e| invalid(e.to_string()))?;
        self.doc = text
            .parse()
            .map_err(|e: toml_edit::TomlError| invalid(e.to_string()))?;
        self.config = config;
        self.sync()
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
