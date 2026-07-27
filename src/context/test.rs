use super::*;

#[test]
fn no_frontmatter_is_the_whole_body_and_inlines() {
    let text = "# Title\n\nBody paragraph.\n";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.frontmatter, None);
    assert_eq!(split.body, text);
    assert_eq!(split.mode("file"), Mode::Inline);
}

#[test]
fn frontmatter_with_description_lists_and_splits_the_body() {
    let text = "---\nname: verify\ndescription: Drive the flow end to end.\n---\nThe body.\n";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.frontmatter,
        Some(Frontmatter {
            name: Some("verify".into()),
            description: Some("Drive the flow end to end.".into()),
            inline: None,
        })
    );
    assert_eq!(split.body, "The body.\n");
    assert_eq!(
        split.mode("fallback"),
        Mode::Listed {
            name: "verify".into(),
            description: "Drive the flow end to end.".into(),
        }
    );
}

#[test]
fn description_without_name_uses_the_fallback() {
    let text = "---\ndescription: A thing.\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.mode("my-doc"),
        Mode::Listed {
            name: "my-doc".into(),
            description: "A thing.".into(),
        }
    );
}

#[test]
fn inline_true_overrides_a_description_back_to_inline() {
    let text = "---\ndescription: A thing.\ninline: true\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.frontmatter.as_ref().unwrap().inline, Some(true));
    assert_eq!(split.mode("d"), Mode::Inline);
}

#[test]
fn frontmatter_without_a_description_inlines() {
    // A name but no description: nothing to catalog, so it inlines.
    let text = "---\nname: just-a-name\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.mode("d"), Mode::Inline);
}

#[test]
fn a_dash_rule_mid_document_is_not_frontmatter() {
    // The file does not *start* with the delimiter, so the `---` below is a horizontal rule.
    let text = "# Title\n\n---\n\nMore text.\n";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.frontmatter, None);
    assert_eq!(split.body, text);
}

#[test]
fn unterminated_frontmatter_is_an_error() {
    let text = "---\nname: oops\nno closing delimiter\n";
    assert_eq!(split_frontmatter(text), Err(UnterminatedFrontmatter));
}

#[test]
fn a_value_may_contain_colons_and_be_quoted() {
    let text = "---\ndescription: \"Use this: for X, Y, and Z\"\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.frontmatter.unwrap().description.as_deref(),
        Some("Use this: for X, Y, and Z"),
    );
}

/// The shape real Claude-Code skill files use: `description: >-` folded over several
/// indented lines. The folded value joins with spaces and still drives listed mode.
#[test]
fn folded_block_scalar_description_lists() {
    let text = "---\nname: ferritin\ndescription: >-\n  Look up real Rust documentation\n  \
                    instead of guessing APIs.\n---\nbody\n";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.mode("fallback"),
        Mode::Listed {
            name: "ferritin".into(),
            description: "Look up real Rust documentation instead of guessing APIs.".into(),
        }
    );
    assert_eq!(split.body, "body\n");
}

/// A literal (`|`) block scalar keeps its line breaks; the chomp indicator is ignored.
#[test]
fn literal_block_scalar_keeps_newlines() {
    let text = "---\ndescription: |-\n  line one\n  line two\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.frontmatter.unwrap().description.as_deref(),
        Some("line one\nline two")
    );
}

/// An unknown key's block value is consumed whole — an indented `description:` line inside
/// it is that block's content, never a frontmatter key of its own.
#[test]
fn unknown_key_block_scalar_cannot_pollute_known_keys() {
    let text = "---\nfuture_key: >-\n  description: sneaky\n  more text\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.mode("d"), Mode::Inline);
    assert_eq!(split.frontmatter.unwrap().description, None);
}

/// A block scalar closed by the frontmatter terminator itself (no flush-left line after it)
/// still lands.
#[test]
fn block_scalar_closed_by_the_terminator() {
    let text = "---\ndescription: >\n  right at the end\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(
        split.frontmatter.unwrap().description.as_deref(),
        Some("right at the end")
    );
}

/// A nested map's interior lines are indented, so its `name:`/`description:` never pollute the
/// top-level keys — the shape of a memory file's `metadata:` map.
#[test]
fn nested_map_keys_cannot_pollute_top_level_keys() {
    let text = "---\nmetadata:\n  name: sneaky\n  type: user\ndescription: Real one.\n---\nb";
    let split = split_frontmatter(text).unwrap();
    let fm = split.frontmatter.unwrap();
    assert_eq!(fm.name, None);
    assert_eq!(fm.description.as_deref(), Some("Real one."));
}

#[test]
fn unknown_frontmatter_keys_are_ignored() {
    let text = "---\ndescription: A.\nfuture_key: whatever\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    let fm = split.frontmatter.unwrap();
    assert_eq!(fm.description.as_deref(), Some("A."));
    assert_eq!(fm.name, None);
    assert_eq!(fm.inline, None);
}

#[test]
fn empty_frontmatter_block_inlines() {
    let text = "---\n---\nbody";
    let split = split_frontmatter(text).unwrap();
    assert_eq!(split.frontmatter, Some(Frontmatter::default()));
    assert_eq!(split.body, "body");
    assert_eq!(split.mode("d"), Mode::Inline);
}

#[test]
fn fallback_name_is_the_stem() {
    assert_eq!(fallback_name(Path::new("docs/verify.md")), "verify");
    assert_eq!(fallback_name(Path::new("CLAUDE.md")), "CLAUDE");
}

// --- render: the name-as-tag pseudo-XML format (byte-pinned) ---

#[test]
fn empty_resolved_renders_none() {
    assert_eq!(Resolved::default().render(), None);
}

/// The full render pins the tagged bytes: a bare inlined file is its own `<name>` tag; a labeled
/// group wraps its listed member(s); sections join with a blank line, in document order. Pins the
/// structure by referencing `LEAD_IN`, so the lead-in wording can change without breaking it.
#[test]
fn render_pins_the_tagged_bytes() {
    let resolved = Resolved {
        sections: vec![
            Section {
                heading: None,
                items: vec![Item::Inline {
                    name: "CLAUDE.md".into(),
                    path: "CLAUDE.md".into(),
                    body: "# efference\n\nThe harness.".into(),
                }],
            },
            Section {
                heading: Some("Skills".into()),
                items: vec![Item::Listed {
                    name: "verify".into(),
                    description: "Drive the flow end to end.".into(),
                    path: "skills/verify.md".into(),
                }],
            },
        ],
    };
    let expected = format!(
        "{LEAD_IN}\n\n\
             <CLAUDE.md>\n# efference\n\nThe harness.\n</CLAUDE.md>\n\n\
             <Skills>\n<verify path=\"skills/verify.md\">Drive the flow end to end.</verify>\n</Skills>"
    );
    assert_eq!(resolved.render().as_deref(), Some(expected.as_str()));
    let resolved = Resolved {
        sections: vec![
            Section {
                heading: None,
                items: vec![Item::Inline {
                    name: "CLAUDE.md".into(),
                    path: "./CLAUDE.md".into(),
                    body: "# efference\n\nThe harness.\n".into(),
                }],
            },
            Section {
                heading: Some("Skills".into()),
                items: vec![Item::Listed {
                    name: "verify".into(),
                    description: "Drive the flow end to end.".into(),
                    path: "skills/verify.md".into(),
                }],
            },
        ],
    };
    let expected = format!(
        "{LEAD_IN}\n\n\
             <CLAUDE.md>\n# efference\n\nThe harness.\n</CLAUDE.md>\n\n\
             <Skills>\n<verify path=\"skills/verify.md\">Drive the flow end to end.</verify>\n</Skills>"
    );
    assert_eq!(resolved.render().as_deref(), Some(expected.as_str()));
}

/// The operator's heading is the tag verbatim — spaces and all, since it's pseudo-XML nothing
/// parses.
#[test]
fn render_uses_the_heading_verbatim_as_a_tag() {
    let resolved = Resolved {
        sections: vec![Section {
            heading: Some("Project Tips".into()),
            items: vec![Item::Listed {
                name: "style".into(),
                description: "Two-space indent.".into(),
                path: "tips/style.md".into(),
            }],
        }],
    };
    assert!(resolved.render().unwrap().contains(
        "<Project Tips>\n<style path=\"tips/style.md\">Two-space indent.</style>\n</Project Tips>"
    ));
}

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
    }
}

fn labeled(heading: &str, paths: &[&str]) -> ContextEntry {
    ContextEntry {
        heading: Some(heading.into()),
        source: ContextSource::Files(paths.iter().map(|s| s.to_string()).collect()),
        grants: ContextGrants::default(),
        sort: None,
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

fn exec_entry(label: &str, command: &str) -> ContextEntry {
    ContextEntry {
        heading: Some(label.into()),
        source: ContextSource::Exec(command.into()),
        grants: ContextGrants::default(),
        sort: None,
    }
}

/// An exec entry is just another row: its stdout becomes a section at its position in
/// document order — a dynamic section sitting *between* static files — collapsed like a
/// labeled single file (the label is the tag, no wrapper).
#[test]
fn resolve_exec_section_lands_in_document_order() {
    let dir = tmpdir("exec-order");
    std::fs::write(dir.join("a.md"), "first\n").unwrap();
    std::fs::write(dir.join("b.md"), "last\n").unwrap();
    let resolved = resolve(
        &[
            bare("a.md"),
            exec_entry("Date", "echo from a command"),
            bare("b.md"),
        ],
        &dir,
    )
    .unwrap();
    assert_eq!(resolved.sections.len(), 3);
    assert_eq!(
        resolved.sections[1],
        Section {
            heading: None,
            items: vec![Item::Exec {
                name: "Date".into(),
                command: "echo from a command".into(),
                body: "from a command\n".into(),
            }],
        }
    );
    let rendered = resolved.render().unwrap();
    let date = rendered.find("<Date>\nfrom a command\n</Date>").unwrap();
    assert!(rendered.find("first").unwrap() < date && date < rendered.find("last").unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The command runs with the session cwd, so relative commands mean what they mean in the
/// bash tool; empty stdout is an empty body, not an error (bytes are bytes).
#[test]
fn resolve_exec_runs_in_base_and_allows_empty_stdout() {
    let dir = tmpdir("exec-cwd");
    std::fs::write(dir.join("marker.txt"), "here\n").unwrap();
    let resolved = resolve(
        &[
            exec_entry("Files", "ls marker.txt"),
            exec_entry("Quiet", "true"),
        ],
        &dir,
    )
    .unwrap();
    assert!(matches!(
        &resolved.sections[0].items[0],
        Item::Exec { body, .. } if body == "marker.txt\n"
    ));
    assert!(matches!(
        &resolved.sections[1].items[0],
        Item::Exec { body, .. } if body.is_empty()
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Fail closed, loudly: a non-zero exit is a config error carrying the command's stderr —
/// never a silently dropped section.
#[test]
fn exec_nonzero_exit_fails_closed_with_stderr() {
    let dir = tmpdir("exec-fail");
    let err = resolve(&[exec_entry("Broken", "echo oops >&2; exit 3")], &dir).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("exit status: 3") && msg.contains("oops"),
        "{msg}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A hung command can't hang boot: past the timeout the process group is killed and the
/// load fails closed.
#[test]
fn exec_timeout_fails_closed() {
    let dir = tmpdir("exec-hang");
    let err = run_exec("sleep 30", &dir, std::time::Duration::from_millis(100)).unwrap_err();
    assert!(err.to_string().contains("timed out"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Binary stdout in a text preamble is a config mistake — fail closed, don't mangle.
#[test]
fn exec_non_utf8_stdout_fails_closed() {
    let dir = tmpdir("exec-binary");
    let err = resolve(&[exec_entry("Binary", r"printf '\xff\xfe'")], &dir).unwrap_err();
    assert!(err.to_string().contains("not valid UTF-8"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}
