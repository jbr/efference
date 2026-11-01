use super::*;

/// The words as `(word, resolved or "?", written)`, asserting the list is complete.
fn words(command: &str) -> Vec<(String, String, bool)> {
    let found = path_words(command, &Directories::at(Path::new("/work/repo")), None);
    assert!(found.complete, "{command}");
    listed(found)
}

fn listed(found: PathWords) -> Vec<(String, String, bool)> {
    found
        .words
        .into_iter()
        .map(|w| {
            let resolved = w.resolved.map_or("?".into(), |r| r.display().to_string());
            (w.word, resolved, w.written)
        })
        .collect()
}

fn word(word: &str, resolved: &str, written: bool) -> (String, String, bool) {
    (word.into(), resolved.into(), written)
}

#[test]
fn a_heredoc_append_names_its_target_and_not_its_body() {
    // The first-run gate: the body mentions `.efference/`, which is data, not a path.
    let command =
        "mkdir -p .git/info && cat >> .git/info/exclude << 'EOF'\n.efference/\nAGENTS.md\nEOF";
    assert_eq!(
        words(command),
        vec![
            word(".git/info", "/work/repo/.git/info", false),
            word(".git/info/exclude", "/work/repo/.git/info/exclude", true),
        ]
    );
}

#[test]
fn a_literal_cd_moves_where_later_words_resolve() {
    assert_eq!(
        words("cd ../other && cat src/lib.rs > ../repo/out.txt"),
        vec![
            word("../other", "/work/other", false),
            word("src/lib.rs", "/work/other/src/lib.rs", false),
            word("../repo/out.txt", "/work/repo/out.txt", true),
        ]
    );
}

#[test]
fn after_a_non_literal_cd_paths_are_listed_unresolved() {
    assert_eq!(
        words("ls ./a && cd $HOME && cat ./b"),
        vec![
            word("./a", "/work/repo/a", false),
            word("$HOME", "?", false),
            word("./b", "?", false),
        ]
    );
}

/// Dropping a word that expands at run time made the list read as complete: a
/// reviewer shown only the in-tree path cleared a read under `~/.cargo`.
#[test]
fn a_word_that_expands_at_run_time_is_listed_unresolved() {
    assert_eq!(
        words("rg -n reader http/src/a.rs; rg fn ~/.cargo/registry/src/*/io.rs > \"$TMPDIR/out\""),
        vec![
            word("http/src/a.rs", "/work/repo/http/src/a.rs", false),
            word("~/.cargo/registry/src/*/io.rs", "?", false),
            word("\"$TMPDIR/out\"", "?", true),
        ]
    );
}

#[test]
fn unread_words_mark_the_list_incomplete() {
    let compound = path_words(
        "for f in src/*.rs; do cat \"$f\"; done",
        &Directories::at(Path::new("/w")),
        None,
    );
    assert!(!compound.complete);
    let many = (0..20)
        .map(|i| format!("./f{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let long = path_words(
        &format!("cat {many}"),
        &Directories::at(Path::new("/w")),
        None,
    );
    assert!(!long.complete);
    assert_eq!(long.words.len(), LIMIT);
    assert!(!path_words("cat 'unterminated", &Directories::at(Path::new("/w")), None).complete);
}

#[test]
fn flags_urls_phrases_and_plain_words_are_not_paths() {
    assert_eq!(
        words("git commit -m 'fix a/b' --file=x/y && curl https://x.test/a && echo hi"),
        Vec::new()
    );
}

#[test]
fn a_repeated_word_is_listed_once_and_keeps_its_write() {
    assert_eq!(
        words("cat ./notes.md && echo x >> ./notes.md"),
        vec![word("./notes.md", "/work/repo/notes.md", true)]
    );
}

#[test]
fn an_unparseable_command_names_nothing() {
    assert_eq!(
        listed(path_words(
            "cat 'unterminated",
            &Directories::at(Path::new("/w")),
            None
        )),
        Vec::new()
    );
}

#[test]
fn devices_are_not_listed() {
    assert_eq!(
        words("ls ./a 2>/dev/null > /dev/stdout"),
        vec![word("./a", "/work/repo/a", false)]
    );
}

fn decided(toml: &str, command: &str) -> Vec<String> {
    let role = Config::from_toml(toml).expect("valid config");
    let cwd = &Directories::at(Path::new("/work/repo"));
    path_words(command, cwd, None)
        .words
        .iter()
        .map(|word| match decide(&role, word, cwd, None) {
            Decided::Grant(grant, perms) => format!("{} {}", grant.display(), perms.write),
            Decided::NoGrant => "none".into(),
            Decided::Unresolved => "?".into(),
            Decided::Straddles(prefix) => format!("straddles {}", prefix.display()),
        })
        .collect()
}

const GRANTS: &str = r#"
[paths]
"." = true
".efference" = false
"#;

/// A pattern is decided by its fixed prefix, unless a more specific grant sits
/// below that prefix where the pattern could match it.
#[test]
fn a_pattern_is_decided_by_its_prefix_unless_it_can_reach_a_carve_out() {
    assert_eq!(
        decided(
            GRANTS,
            "cat src/*.rs ./*.md .e*/log.jsonl ~/x .efference/*/log.jsonl"
        ),
        vec![
            "/work/repo true",
            // `*` does not match a dot-name, so `.efference` is out of its reach.
            "/work/repo true",
            "straddles /work/repo",
            "?",
            "/work/repo/.efference false",
        ]
    );
}

#[test]
fn a_literal_path_is_decided_by_its_deepest_grant() {
    assert_eq!(
        decided(GRANTS, "cat ./a .efference/log /etc/hosts"),
        vec!["/work/repo true", "/work/repo/.efference false", "none"]
    );
}

/// The harness sets `$TMPDIR` to the scratch directory, so a path under it
/// resolves there and is decided by the scratch grant, not listed unresolved.
#[test]
fn a_tmpdir_path_resolves_under_the_scratch_directory() {
    let role = Config::from_toml(GRANTS).expect("valid config");
    let cwd = &Directories::at(Path::new("/work/repo"));
    let scratch = Path::new("/tmp/scratch-1");
    let found = path_words(
        "cat \"$TMPDIR/models.json\" > ${TMPDIR}/out && cat $TMPDIR/$NAME",
        cwd,
        Some(scratch),
    );
    let facts: Vec<_> = found
        .words
        .iter()
        .map(|word| {
            let resolved = word.resolved.as_ref().map(|r| r.display().to_string());
            let grant = match decide(&role, word, cwd, Some(scratch)) {
                Decided::Grant(grant, perms) => format!("{} {}", grant.display(), perms.write),
                other => format!("{other:?}"),
            };
            (resolved, grant)
        })
        .collect();
    assert_eq!(
        facts,
        vec![
            (
                Some("/tmp/scratch-1/models.json".into()),
                "/tmp/scratch-1 true".into()
            ),
            (
                Some("/tmp/scratch-1/out".into()),
                "/tmp/scratch-1 true".into()
            ),
            (None, "Unresolved".into()),
        ]
    );
}
