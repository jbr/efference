use super::*;

// --- render: the name-as-tag pseudo-XML format (byte-pinned) ---

// --- resolve: config entries → files on disk → Resolved (docs/context.md) ---

use crate::config::ContextGrants;

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("efference-ctx-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn bare(path: &str) -> ContextEntry {
    ContextEntry {
        heading: None,
        source: ContextSource::Files(vec![path.into()]),
        grants: ContextGrants::default(),
        sort: None,
        description: None,
    }
}

fn labeled(heading: &str, paths: &[&str]) -> ContextEntry {
    ContextEntry {
        heading: Some(heading.into()),
        source: ContextSource::Files(paths.iter().map(|s| s.to_string()).collect()),
        grants: ContextGrants::default(),
        sort: None,
        description: None,
    }
}

#[test]
fn resolve_bare_inline_file_uses_the_basename() {
    let dir = tmpdir("bare-inline");
    std::fs::write(dir.join("CLAUDE.md"), "# efference\nbody\n").unwrap();
    let resolved = resolve(&[bare("CLAUDE.md")], &dir).unwrap();
    assert_eq!(
        resolved.sections,
        vec![Section {
            heading: None,
            description: None,
            items: vec![Item::Inline {
                name: "CLAUDE".into(),
                path: "CLAUDE.md".into(),
                body: "# efference\nbody\n".into(),
            }],
        }]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Skills-compat: a file with `name` + `description` becomes a *listed* stub tagged by its
/// frontmatter name (preferred over the basename).
#[test]
fn resolve_prefers_frontmatter_name_and_lists_a_described_file() {
    let dir = tmpdir("fm-name");
    std::fs::write(
        dir.join("v.md"),
        "---\nname: verify\ndescription: Drive it.\n---\nbody\n",
    )
    .unwrap();
    let resolved = resolve(&[bare("v.md")], &dir).unwrap();
    assert_eq!(
        resolved.sections,
        vec![Section {
            heading: None,
            description: None,
            items: vec![Item::Listed {
                name: "verify".into(),
                description: "Drive it.".into(),
                path: "v.md".into(),
            }],
        }]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A labeled entry resolving to a single file collapses: the label is the file's tag, no wrapper.
#[test]
fn resolve_labeled_single_file_collapses_label_to_the_tag() {
    let dir = tmpdir("labeled-single");
    std::fs::write(dir.join("arch.md"), "the architecture\n").unwrap();
    let resolved = resolve(&[labeled("Architecture", &["arch.md"])], &dir).unwrap();
    assert_eq!(
        resolved.sections,
        vec![Section {
            heading: None,
            description: None,
            items: vec![Item::Inline {
                name: "Architecture".into(),
                path: "arch.md".into(),
                body: "the architecture\n".into(),
            }],
        }]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A labeled entry resolving to many files (a directory) wraps its `.md` children, name-sorted,
/// each tagged individually.
#[test]
fn resolve_labeled_dir_wraps_sorted_members() {
    let dir = tmpdir("labeled-dir");
    let skills = dir.join("skills");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(
        skills.join("release.md"),
        "---\nname: release\ndescription: Cut a release.\n---\nb\n",
    )
    .unwrap();
    std::fs::write(
        skills.join("verify.md"),
        "---\nname: verify\ndescription: Drive the flow.\n---\nb\n",
    )
    .unwrap();
    // A non-markdown file in the dir is ignored.
    std::fs::write(skills.join("notes.txt"), "ignore me").unwrap();
    let resolved = resolve(&[labeled("Skills", &["skills"])], &dir).unwrap();
    assert_eq!(
        resolved.sections,
        vec![Section {
            heading: Some("Skills".into()),
            description: None,
            items: vec![
                Item::Listed {
                    name: "release".into(),
                    description: "Cut a release.".into(),
                    path: "skills/release.md".into(),
                },
                Item::Listed {
                    name: "verify".into(),
                    description: "Drive the flow.".into(),
                    path: "skills/verify.md".into(),
                },
            ],
        }]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The Claude-Code skills layout — one directory per skill holding a `SKILL.md` — lands as
/// one listing per skill via the recursive walk. Hidden children (a `.cache` dir, a dotfile)
/// are skipped; the walk is sorted by display path; and a symlink cycle (here, a link back
/// to the root) is walked once, not forever — the cycle guard skips the re-entry.
#[test]
fn resolve_dir_walks_recursively_for_the_skills_layout() {
    let dir = tmpdir("skills-tree");
    let skills = dir.join("skills");
    std::fs::create_dir_all(skills.join("alpha")).unwrap();
    std::fs::create_dir_all(skills.join("beta")).unwrap();
    std::fs::create_dir_all(skills.join(".cache")).unwrap();
    std::fs::write(
        skills.join("alpha/SKILL.md"),
        "---\nname: alpha\ndescription: First.\n---\nb\n",
    )
    .unwrap();
    std::fs::write(
        skills.join("beta/SKILL.md"),
        "---\nname: beta\ndescription: Second.\n---\nb\n",
    )
    .unwrap();
    std::fs::write(skills.join(".cache/ignored.md"), "not content\n").unwrap();
    std::fs::write(skills.join(".hidden.md"), "not content\n").unwrap();
    std::os::unix::fs::symlink(&skills, skills.join("loop")).unwrap();

    let resolved = resolve(&[labeled("Skills", &["skills"])], &dir).unwrap();
    assert_eq!(
        resolved.sections,
        vec![Section {
            heading: Some("Skills".into()),
            description: None,
            items: vec![
                Item::Listed {
                    name: "alpha".into(),
                    description: "First.".into(),
                    path: "skills/alpha/SKILL.md".into(),
                },
                Item::Listed {
                    name: "beta".into(),
                    description: "Second.".into(),
                    path: "skills/beta/SKILL.md".into(),
                },
            ],
        }]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A symlinked directory inside a listed tree is a real layout (a skill collection linked in
/// from another repo) — the walk follows it, and files land under the *link's* display path,
/// the path the operator's tree presents.
#[test]
fn resolve_follows_a_symlinked_directory() {
    let dir = tmpdir("skills-symlink");
    let skills = dir.join("skills");
    let external = dir.join("external");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::create_dir_all(&external).unwrap();
    std::fs::write(
        external.join("SKILL.md"),
        "---\nname: linked\ndescription: From elsewhere.\n---\nb\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&external, skills.join("linked")).unwrap();

    let resolved = resolve(&[labeled("Skills", &["skills"])], &dir).unwrap();
    assert_eq!(
        resolved.sections,
        vec![Section {
            // Resolving to a single file, the entry collapses: the operator's label is the
            // tag (label > frontmatter name — the settled precedence), no wrapper.
            heading: None,
            items: vec![Item::Listed {
                name: "Skills".into(),
                description: "From elsewhere.".into(),
                path: "skills/linked/SKILL.md".into(),
            }],
            description: None,
        }]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn resolve_missing_path_fails_closed() {
    let dir = tmpdir("missing");
    let err = resolve(&[bare("nope.md")], &dir).unwrap_err();
    assert!(matches!(err, ResolveError::Io(..)));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn resolve_unterminated_frontmatter_fails_closed() {
    let dir = tmpdir("unterm");
    std::fs::write(dir.join("bad.md"), "---\nname: x\nno close\n").unwrap();
    let err = resolve(&[bare("bad.md")], &dir).unwrap_err();
    assert!(matches!(err, ResolveError::Frontmatter(_)));
    std::fs::remove_dir_all(&dir).unwrap();
}

// --- exec sections (docs/context.md) ---

mod exec;
mod frontmatter;
mod render;
