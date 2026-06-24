//! Session environment context for the system preamble (DESIGN §3, orientation).
//!
//! A weak model that isn't told *where* it is guesses — a live Haiku run searched
//! for Python/JS/TS files in a Rust repo before finding its footing. This module
//! builds a short block of concrete facts (working directory, platform,
//! project type) to append to the system prompt so the agent is oriented from
//! turn one.
//!
//! **Caching discipline (load-bearing).** The system prompt sits in the cached
//! prefix (`tools → system → messages`), so anything injected here must be
//! **byte-stable for the whole session** or it invalidates the cache every turn.
//! Therefore [`context`] is computed **once at startup** and frozen into the
//! prompt, and it reports only session-stable facts. Volatile state (git status,
//! file listings, `cargo` output) is deliberately excluded — that is the agent's
//! to fetch via tools, where it lands past the cache breakpoint.

use std::path::Path;

/// Build the environment block appended to the system preamble. Call **once** at
/// session start; never recompute (see the caching note above).
pub fn context() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let project = detect_project(Path::new(&cwd));
    format!(
        "Environment (fixed for this session):\n\
         - working directory: {cwd}\n\
         - platform: {os} ({arch})\n\
         - project type: {project}"
    )
}

/// Marker files that identify a project's ecosystem, most-specific first. Pure
/// over the filesystem so it stays deterministic for the session.
const MARKERS: &[(&str, &str)] = &[
    ("Cargo.toml", "Rust (Cargo)"),
    ("package.json", "Node.js"),
    ("pyproject.toml", "Python"),
    ("setup.py", "Python"),
    ("requirements.txt", "Python"),
    ("go.mod", "Go"),
    ("pom.xml", "Java (Maven)"),
    ("build.gradle", "Java/Kotlin (Gradle)"),
    ("Gemfile", "Ruby"),
    ("composer.json", "PHP"),
    ("mix.exs", "Elixir"),
];

/// Name the project ecosystem(s) by marker files in `dir`. Multiple matches join
/// (a polyglot repo), deduplicated so two Python markers read as one. Unknown
/// when nothing matches — honest, not a guess.
fn detect_project(dir: &Path) -> String {
    let mut names: Vec<&str> = Vec::new();
    for (file, name) in MARKERS {
        if dir.join(file).exists() && !names.contains(name) {
            names.push(name);
        }
    }
    if names.is_empty() {
        "unknown".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("efference-env-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn detects_rust_by_cargo_toml() {
        let dir = tmpdir("rust");
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_project(&dir), "Rust (Cargo)");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dedups_two_python_markers_into_one_name() {
        let dir = tmpdir("py");
        std::fs::write(dir.join("pyproject.toml"), "").unwrap();
        std::fs::write(dir.join("requirements.txt"), "").unwrap();
        assert_eq!(detect_project(&dir), "Python");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn polyglot_joins_distinct_ecosystems() {
        let dir = tmpdir("poly");
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        assert_eq!(detect_project(&dir), "Rust (Cargo), Node.js");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_when_no_markers() {
        let dir = tmpdir("bare");
        assert_eq!(detect_project(&dir), "unknown");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detects_elixir_by_mix_exs() {
        let dir = tmpdir("elixir");
        std::fs::write(dir.join("mix.exs"), "defmodule MyApp.MixProject do").unwrap();
        assert_eq!(detect_project(&dir), "Elixir");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
