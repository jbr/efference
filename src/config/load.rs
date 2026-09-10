//! Resolving and loading a config file, and how loading fails (docs/permissions.md).

use super::*;

/// Where a [`Config`] was resolved from, reported on `GET /state` so "which config am I
/// running?" is never ambiguous (docs/permissions.md).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ConfigSource {
    /// No config file found — the built-in [`Config::default`] is in effect.
    #[default]
    Default,
    /// Resolved from this path (an explicit `--config`, or the default path).
    Path(PathBuf),
}

/// A resolved config plus where it came from. Stored on the daemon's shared state and
/// reported by `/state`.
#[derive(Debug, Clone, Default)]
pub struct LoadedConfig {
    pub role: Config,
    pub source: ConfigSource,
}

/// Failure to load a config file.
#[derive(Debug)]
pub enum LoadError {
    /// Reading the file failed (an explicit `--config` that doesn't exist, a permission
    /// error, …). A *missing default* path is not an error — it yields [`ConfigSource::Default`].
    Io(PathBuf, std::io::Error),
    /// The file is not valid TOML / does not match the schema. The [`toml_edit`] error carries
    /// the source span; its `Display` renders the offending line and a pointer.
    Parse(PathBuf, toml_edit::de::Error),
    /// The file parsed but is semantically unsound (see [`ValidationError`]).
    Validate(PathBuf, ValidationError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(path, e) => write!(f, "reading config {}: {e}", path.display()),
            LoadError::Parse(path, e) => write!(f, "invalid config {}: {e}", path.display()),
            LoadError::Validate(path, e) => write!(f, "invalid config {}: {e}", path.display()),
        }
    }
}

impl std::error::Error for LoadError {}

impl LoadError {
    /// Print this error to stderr. TOML parse errors render their own line/column context
    /// (the source-span pointer that stands in for KDL's old miette report); the others print
    /// their message. The leading `error:` keeps the shape the binary used before.
    pub fn eprint(&self) {
        eprintln!("error: {self}");
    }
}

/// Resolve and load the config. With `explicit` (a `--config` path) the file *must* exist —
/// a missing one is an error. Without it, the [`DEFAULT_PATH`] is probed: if present it loads,
/// if absent the built-in [`Config::default`] is used ([`ConfigSource::Default`]).
pub fn load(explicit: Option<&Path>) -> Result<LoadedConfig, LoadError> {
    load_in(Path::new(""), explicit)
}

/// Load an optional template relative to an explicit session root (docs/permissions.md).
/// An absent default still selects the built-in configuration; an absent explicit
/// path remains an error.
pub fn load_in(cwd: &Path, explicit: Option<&Path>) -> Result<LoadedConfig, LoadError> {
    let path = cwd.join(explicit.unwrap_or_else(|| Path::new(DEFAULT_PATH)));
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(LoadedConfig {
            role: parse(&path, &text)?,
            source: ConfigSource::Path(path),
        }),
        Err(e) if explicit.is_none() && e.kind() == std::io::ErrorKind::NotFound => {
            Ok(LoadedConfig::default())
        }
        Err(e) => Err(LoadError::Io(path, e)),
    }
}
