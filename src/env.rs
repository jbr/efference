//! Session environment context for the system preamble (docs/substrate.md).
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

/// The system preamble. Explains the curation model so the agent stops defending
/// against context bloat the old way (docs/redaction-fork.md) — the run that
/// motivated this watched the model reflexively `grep`/`head` its own tool output.
/// The prose lives in `assets/prompts/session.md` so it can be edited as markdown
/// rather than an escaped Rust literal (the same `include_str!` pattern the spotter
/// persona uses, see [`crate::spotter`]); [`system_prompt`] trims its trailing
/// newline so the joined prompt stays byte-clean.
const PREAMBLE: &str = include_str!("../assets/prompts/session.md");

/// The full system prompt sent in the request's `system` field (docs/substrate.md): the
/// curation-model [`PREAMBLE`], the frozen session environment block from [`context`],
/// and — when the session runs under a role with a brief — the role's preamble section
/// (`role_section`, rendered by [`crate::role::Role::preamble_section`]). Built once at
/// session start and frozen into the cached prefix, so every part must be byte-stable;
/// the `wire` dev view reconstructs it identically by calling this with the same inputs.
pub fn system_prompt(role_section: Option<&str>) -> String {
    // `include_str!` keeps the file's trailing newline; trim it so the join is clean.
    let mut prompt = format!("{}\n\n{}", PREAMBLE.trim_end(), context());
    if let Some(section) = role_section {
        prompt.push_str("\n\n");
        prompt.push_str(section);
    }
    prompt
}

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
    fn system_prompt_appends_role_section_when_present() {
        let without = system_prompt(None);
        let with = system_prompt(Some("Your role for this session:\n\nStay put."));
        // The role section is appended verbatim after the env block; without a role the
        // prompt is exactly the preamble + env (no trailing role text).
        assert!(with.starts_with(&without));
        assert!(with.ends_with("Your role for this session:\n\nStay put."));
        assert!(!without.contains("Your role for this session"));
    }

    #[test]
    fn detects_elixir_by_mix_exs() {
        let dir = tmpdir("elixir");
        std::fs::write(dir.join("mix.exs"), "defmodule MyApp.MixProject do").unwrap();
        assert_eq!(detect_project(&dir), "Elixir");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
