//! A path with the home directory written as `~`, and the reverse: a typed `~`
//! read as the home directory, and a client's directory canonicalized as the host
//! keys it (docs/tools.md, docs/control-api.md).

use std::path::{Path, PathBuf};

/// `path` with the home directory written as `~`, when `$HOME` is a prefix of a
/// whole path component; any other path as written.
pub fn tilde(path: &Path) -> String {
    under_home(path, std::env::var_os("HOME").as_deref().map(Path::new))
}

/// [`tilde`] against a given home, so the rewrite is testable without touching
/// the process environment.
pub fn under_home(path: &Path, home: Option<&Path>) -> String {
    let display = path.display().to_string();
    let Some(home) = home else {
        return display;
    };
    let home = home.display().to_string();
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return display;
    }
    match display.strip_prefix(home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => display,
    }
}

/// `path` with a leading `~`, alone or followed by `/`, read as the home directory;
/// any other path as written. `~user` is not expanded, and with no home nothing is.
pub fn expand(path: &Path, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return path.to_path_buf();
    };
    match path.strip_prefix("~") {
        Ok(rest) if rest.as_os_str().is_empty() => home.to_path_buf(),
        Ok(rest) => home.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// A directory a client named, spelled as the host keys it: [`expand`] against
/// `home`, then, when the path is absolute and resolves, canonicalized. Two
/// spellings of one directory — through a symlink, or `/users` for `/Users` on a
/// case-insensitive filesystem — reach the one root that the session register and
/// the remembered locations key on (docs/control-api.md). A path that does not
/// resolve keeps its expanded spelling, and a relative one stays relative, so the
/// caller's absoluteness check still refuses it rather than reading it against the
/// host's own working directory.
pub fn canonical(path: &Path, home: Option<&Path>) -> PathBuf {
    let path = expand(path, home);
    if !path.is_absolute() {
        return path;
    }
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// [`canonical`] against this process's `$HOME`: the host's own home, which is where
/// a client's `~` points because the path names a directory on the host.
pub fn canonical_here(path: &Path) -> PathBuf {
    canonical(path, std::env::var_os("HOME").as_deref().map(Path::new))
}

#[cfg(test)]
mod test;
