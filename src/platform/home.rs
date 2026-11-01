//! A path with the home directory written as `~` (docs/tools.md).

use std::path::Path;

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

#[cfg(test)]
mod test;
