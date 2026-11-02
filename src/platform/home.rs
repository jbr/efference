//! A path with the home directory written as `~`, and the reverse: a typed `~`
//! read as the home directory (docs/tools.md, docs/control-api.md).

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

/// [`expand`] against this process's `$HOME`: the host's own home, which is where a
/// client's `~` points because the path names a directory on the host.
pub fn expand_here(path: &Path) -> PathBuf {
    expand(path, std::env::var_os("HOME").as_deref().map(Path::new))
}

#[cfg(test)]
mod test;
