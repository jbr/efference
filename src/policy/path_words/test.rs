use super::*;

fn words(command: &str) -> Vec<(String, String, bool)> {
    path_words(command, Path::new("/work/repo"))
        .into_iter()
        .map(|w| (w.word, w.resolved.display().to_string(), w.written))
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
fn a_non_literal_cd_stops_the_listing() {
    assert_eq!(
        words("ls ./a && cd $HOME && cat ./b"),
        vec![word("./a", "/work/repo/a", false)]
    );
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
    assert_eq!(words("cat 'unterminated"), Vec::new());
}

#[test]
fn devices_are_not_listed() {
    assert_eq!(
        words("ls ./a 2>/dev/null > /dev/stdout"),
        vec![word("./a", "/work/repo/a", false)]
    );
}
