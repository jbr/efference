//! The words in a shell command that name paths, resolved the way the grant checks
//! resolve them — the facts the spotter is handed beside a bash move
//! (docs/permissions.md). A reviewer model asked to resolve `.git/info/exclude`
//! against a working-directory line and a list of grants misreads it often enough
//! to gate in-tree work; stating where each path lands, and which grant decides it,
//! turns that into a comparison of named facts.
//!
//! This is description, not policy: nothing here clears or gates. A word is listed
//! when it *looks* like a path, so an argument such as `origin/main` can appear;
//! the block is labeled as the harness's reading, and the reviewer still has the
//! command itself.

use super::commands::{literal_word, parser_options};
use super::*;

/// How many path words a move lists at most; past that the block stops being a
/// quick read and the command itself is the better source.
const LIMIT: usize = 12;

/// One path-shaped word, resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathWord {
    /// The word as the command spells it.
    pub word: String,
    /// The word resolved against the directory the command is in at that point
    /// (the working directory, or a literal `cd`'s target), lexically normalized.
    pub resolved: PathBuf,
    /// Whether the word is the target of an output redirection (`>`, `>>`, `>|`,
    /// `<>`, `&>`) — a write whatever the program is.
    pub written: bool,
}

/// The path-shaped words in `command`, in order and without repeats, resolved
/// against `cwd` as the command moves through literal `cd`s. Empty when the command
/// does not parse. Stops at a `cd` whose target is not a literal, since every
/// relative word after it resolves somewhere this cannot say.
pub(crate) fn path_words(command: &str, cwd: &Path) -> Vec<PathWord> {
    let mut walk = Walk {
        dir: Some(cwd.to_path_buf()),
        words: Vec::new(),
    };
    let mut parser = brush_parser::Parser::new(command.as_bytes(), &parser_options());
    if let Ok(program) = parser.parse_program() {
        for list in &program.complete_commands {
            walk.compound_list(list);
        }
    }
    walk.words
}

struct Walk {
    /// The directory relative words resolve against; `None` once a non-literal `cd`
    /// makes it unknowable.
    dir: Option<PathBuf>,
    words: Vec<PathWord>,
}

impl Walk {
    fn compound_list(&mut self, list: &brush_parser::ast::CompoundList) {
        use brush_parser::ast::AndOr;
        for item in &list.0 {
            self.pipeline(&item.0.first);
            for next in &item.0.additional {
                let (AndOr::And(pipeline) | AndOr::Or(pipeline)) = next;
                self.pipeline(pipeline);
            }
        }
    }

    fn pipeline(&mut self, pipeline: &brush_parser::ast::Pipeline) {
        use brush_parser::ast::Command;
        for command in &pipeline.seq {
            // Compound commands, functions, and `[[ … ]]` are left to the reviewer to
            // read: their words are not a flat argument list.
            if let Command::Simple(simple) = command {
                self.simple(simple);
            }
        }
    }

    fn simple(&mut self, simple: &brush_parser::ast::SimpleCommand) {
        use brush_parser::ast::CommandPrefixOrSuffixItem;
        let items = simple
            .prefix
            .iter()
            .flat_map(|prefix| &prefix.0)
            .chain(simple.suffix.iter().flat_map(|suffix| &suffix.0));
        if simple
            .word_or_name
            .as_ref()
            .and_then(literal_word)
            .as_deref()
            == Some("cd")
        {
            self.cd(items);
            return;
        }
        for item in items {
            match item {
                CommandPrefixOrSuffixItem::IoRedirect(redirect) => self.redirect(redirect),
                CommandPrefixOrSuffixItem::Word(word) => {
                    if let Some(argument) = literal_word(word).filter(|a| looks_like_path(a)) {
                        self.push(&argument, false);
                    }
                }
                _ => {}
            }
        }
    }

    /// Where later words resolve: a literal target moves the directory, and anything
    /// else (`cd`, `cd -`, `cd $X`) leaves it unknown.
    fn cd<'a>(
        &mut self,
        items: impl Iterator<Item = &'a brush_parser::ast::CommandPrefixOrSuffixItem>,
    ) {
        use brush_parser::ast::CommandPrefixOrSuffixItem;
        let arguments: Vec<_> = items
            .filter_map(|item| match item {
                CommandPrefixOrSuffixItem::Word(word) => Some(literal_word(word)),
                _ => None,
            })
            .collect();
        let target = match arguments.as_slice() {
            [Some(target)] if target != "-" => Some(target.clone()),
            _ => None,
        };
        if let Some(target) = &target {
            self.push(target, false);
        }
        self.dir = target
            .zip(self.dir.as_deref())
            .map(|(target, dir)| lexical_normalize(&absolutize(&target, dir)));
    }

    fn redirect(&mut self, redirect: &brush_parser::ast::IoRedirect) {
        use brush_parser::ast::{IoFileRedirectKind as Kind, IoFileRedirectTarget, IoRedirect};
        let (word, written) = match redirect {
            IoRedirect::File(_, kind, IoFileRedirectTarget::Filename(word)) => match kind {
                Kind::Read => (word, false),
                Kind::Write | Kind::Append | Kind::Clobber | Kind::ReadAndWrite => (word, true),
                Kind::DuplicateInput | Kind::DuplicateOutput => return,
            },
            IoRedirect::OutputAndError(word, _) => (word, true),
            // A here-doc or here-string is data, and its body is not paths.
            _ => return,
        };
        if let Some(word) = literal_word(word) {
            self.push(&word, written);
        }
    }

    fn push(&mut self, word: &str, written: bool) {
        let Some(dir) = &self.dir else {
            return;
        };
        let resolved = lexical_normalize(&absolutize(word, dir));
        // A device (`2>/dev/null`, `/dev/stdout`) is not a grant question; listed as
        // "no grant covers it", it read as a write outside scope.
        if resolved.starts_with("/dev") {
            return;
        }
        if let Some(seen) = self.words.iter_mut().find(|seen| seen.word == word) {
            seen.written |= written;
            return;
        }
        if self.words.len() < LIMIT {
            self.words.push(PathWord {
                word: word.to_string(),
                resolved,
                written,
            });
        }
    }
}

/// Whether an argument reads as a path: `.`/`..`, a dot-prefixed name, or anything
/// with a `/` — but not a flag, a URL, or a phrase (a commit message with a slash
/// in it).
fn looks_like_path(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with('-')
        && !word.contains("://")
        && !word.contains(char::is_whitespace)
        && (word.starts_with('.') || word.contains('/'))
}

#[cfg(test)]
mod test;
