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
//!
//! The list must not read as complete when it is not. A reviewer shown three
//! resolved in-tree paths takes them as everything the command touches, so a
//! path word that only expands at run time (`~/.cargo/…`, `$HOME/x`, a glob) is
//! listed unresolved rather than dropped — dropping one cleared a read under
//! `~/.cargo` on the decision rung — and [`PathWords::complete`] says when words
//! were left unread (a compound command, the listing limit).

use super::commands::{literal_word, parser_options, pattern_word};
use super::*;

/// How many path words a move lists at most; past that the block stops being a
/// quick read and the command itself is the better source.
const LIMIT: usize = 12;

/// The path-shaped words of one command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PathWords {
    pub words: Vec<PathWord>,
    /// False when words were left unread — a compound command, a function, or
    /// the listing limit — so the list is not everything the command names.
    pub complete: bool,
}

/// One path-shaped word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathWord {
    /// The word as the command spells it.
    pub word: String,
    /// The word resolved against the directory the command is in at that point
    /// (the working directory, or a literal `cd`'s target), lexically normalized.
    /// `None` when that cannot be said here: the word expands when the command
    /// runs (a tilde, a variable, a brace), or follows a `cd` whose target does.
    pub resolved: Option<PathBuf>,
    /// Whether `resolved` is a pathname pattern (`src/*.rs`): it names whatever
    /// matches under its fixed prefix, which [`decide`] accounts for.
    pub pattern: bool,
    /// Whether the word is the target of an output redirection (`>`, `>>`, `>|`,
    /// `<>`, `&>`) — a write whatever the program is.
    pub written: bool,
}

/// The path-shaped words in `command`, in order and without repeats, resolved
/// against the working directory as the command moves through literal `cd`s. Empty and complete
/// when the command names none; empty and incomplete when it does not parse.
///
/// `scratch` is the session's scratch directory, which every tool child sees as
/// `$TMPDIR` (docs/tools.md): a word that is `$TMPDIR` (or `${TMPDIR}`) followed
/// by a literal remainder resolves under it rather than being listed unresolved,
/// since the harness set the variable itself.
pub(crate) fn path_words(command: &str, dirs: &Directories, scratch: Option<&Path>) -> PathWords {
    let mut walk = Walk {
        dir: Some(dirs.working().to_path_buf()),
        scratch: scratch.map(Path::to_path_buf),
        found: PathWords {
            words: Vec::new(),
            complete: true,
        },
    };
    let mut parser = brush_parser::Parser::new(command.as_bytes(), &parser_options());
    match parser.parse_program() {
        Ok(program) => {
            for list in &program.complete_commands {
                walk.compound_list(list);
            }
        }
        Err(_) => walk.found.complete = false,
    }
    walk.found
}

struct Walk {
    /// The directory relative words resolve against; `None` once a non-literal `cd`
    /// makes it unknowable.
    dir: Option<PathBuf>,
    scratch: Option<PathBuf>,
    found: PathWords,
}

/// A word as the walk sees it: its literal value when bash would not expand it,
/// else its source text, which only the running shell can resolve.
#[derive(Clone)]
enum Spelled {
    Literal(String),
    Pattern(String),
    Expands(String),
}

impl Spelled {
    fn text(&self) -> &str {
        match self {
            Spelled::Literal(text) | Spelled::Pattern(text) | Spelled::Expands(text) => text,
        }
    }
}

fn spelled(word: &brush_parser::ast::Word) -> Spelled {
    if let Some(literal) = literal_word(word) {
        return Spelled::Literal(literal);
    }
    match pattern_word(word) {
        Some(pattern) => Spelled::Pattern(pattern),
        None => Spelled::Expands(word.value.clone()),
    }
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
            match command {
                Command::Simple(simple) => self.simple(simple),
                // Compound commands, functions, and `[[ … ]]` are left to the
                // reviewer to read: their words are not a flat argument list.
                _ => self.found.complete = false,
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
                    let word = spelled(word);
                    if looks_like_path(word.text()) {
                        self.push(word, false);
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
                CommandPrefixOrSuffixItem::Word(word) => Some(spelled(word)),
                _ => None,
            })
            .collect();
        let target = match arguments.as_slice() {
            [Spelled::Literal(target)] if target != "-" => Some(target.clone()),
            _ => None,
        };
        if let [argument] = arguments.as_slice() {
            self.push(argument.clone(), false);
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
        self.push(spelled(word), written);
    }

    fn push(&mut self, word: Spelled, written: bool) {
        let resolve = |text: &str| {
            self.dir
                .as_deref()
                .map(|dir| lexical_normalize(&absolutize(text, dir)))
        };
        let (word, resolved, pattern) = match word {
            Spelled::Literal(text) => {
                let resolved = resolve(&text);
                (text, resolved, false)
            }
            Spelled::Pattern(text) => {
                let resolved = resolve(&text);
                (text, resolved, true)
            }
            Spelled::Expands(text) => {
                let resolved = self
                    .scratch
                    .as_deref()
                    .and_then(|scratch| under_scratch(&text, scratch));
                (text, resolved, false)
            }
        };
        // A device (`2>/dev/null`, `/dev/stdout`) is not a grant question; listed as
        // "no grant covers it", it read as a write outside scope.
        if resolved.as_deref().is_some_and(|r| r.starts_with("/dev")) {
            return;
        }
        let words = &mut self.found.words;
        if let Some(seen) = words.iter_mut().find(|seen| seen.word == word) {
            seen.written |= written;
            return;
        }
        if words.len() < LIMIT {
            words.push(PathWord {
                word,
                resolved,
                pattern,
                written,
            });
        } else {
            self.found.complete = false;
        }
    }
}

/// `$TMPDIR/rest`, `${TMPDIR}/rest`, or either in double quotes, resolved under
/// `scratch` when `rest` is wholly literal; `None` for anything else.
fn under_scratch(text: &str, scratch: &Path) -> Option<PathBuf> {
    let text = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(text);
    let rest = text
        .strip_prefix("$TMPDIR")
        .or_else(|| text.strip_prefix("${TMPDIR}"))?;
    if !(rest.is_empty() || rest.starts_with('/'))
        || rest.contains(['$', '`', '\\', '"', '\'', '~', '*', '?', '[', '{'])
    {
        return None;
    }
    Some(lexical_normalize(
        &scratch.join(rest.trim_start_matches('/')),
    ))
}

/// The scratch directory's grant: always read-write (docs/tools.md).
const SCRATCH: Perms = Perms {
    read: true,
    write: true,
};

/// What the grants say about one [`PathWord`] — the fact both spotter rungs state.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decided<'a> {
    /// The deepest grant containing the path (every path a pattern can match).
    Grant(PathBuf, &'a Perms),
    /// No grant contains it.
    NoGrant,
    /// The word expands when the command runs; nothing can be said here.
    Unresolved,
    /// A pattern whose matches may fall under a more specific grant (a carve-out
    /// such as `.efference`) below its fixed prefix, so no single grant decides.
    Straddles(PathBuf),
}

/// Which grant decides `word`. A pattern is decided by its fixed prefix — the
/// components before the first one holding a glob character — unless a more
/// specific grant sits below that prefix where the pattern could match it: `.e*/x`
/// resolves lexically under `.` but can reach a denied `.efference`. A component
/// without a leading `.` does not match a dot-name (bash's default), so `*.md`
/// at the root does not reach `.efference`.
///
/// A path under `scratch` is decided by the scratch grant, which a live session
/// carries in its `[paths]` and an offline replay knows only by the alias.
pub(crate) fn decide<'a>(
    role: &'a Config,
    word: &PathWord,
    dirs: &Directories,
    scratch: Option<&Path>,
) -> Decided<'a> {
    let Some(resolved) = &word.resolved else {
        return Decided::Unresolved;
    };
    if let Some(scratch) = scratch.filter(|scratch| resolved.starts_with(scratch)) {
        return Decided::Grant(scratch.to_path_buf(), &SCRATCH);
    }
    // Words resolve against the working directory (or a `cd`'s); grants judge the project
    // counterpart, mapped once here.
    let resolved = &dirs.judged(resolved);
    let decided = |path: &Path| match governing_grant(role, path, dirs) {
        Some((grant, perms)) => Decided::Grant(grant, perms),
        None => Decided::NoGrant,
    };
    if !word.pattern {
        return decided(resolved);
    }
    let is_glob = |c: &Component| c.as_os_str().to_string_lossy().contains(['*', '?', '[']);
    let components: Vec<Component> = resolved.components().collect();
    let Some(first_glob) = components.iter().position(is_glob) else {
        return decided(resolved);
    };
    let prefix: PathBuf = components[..first_glob].iter().collect();
    let glob = components[first_glob].as_os_str().to_string_lossy();
    let reaches_deeper = role.paths.keys().any(|grant| {
        let grant = dirs.grant(grant);
        let Ok(below) = grant.strip_prefix(&prefix) else {
            return false;
        };
        match below.components().next() {
            Some(first) => {
                let name = first.as_os_str().to_string_lossy();
                glob.starts_with('.') || !name.starts_with('.')
            }
            None => false,
        }
    });
    if reaches_deeper {
        Decided::Straddles(prefix)
    } else {
        decided(&prefix)
    }
}

/// Whether an argument reads as a path: `.`/`..`, a dot- or tilde-prefixed name,
/// or anything with a `/` — but not a flag, a URL, or a phrase (a commit message
/// with a slash in it).
fn looks_like_path(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with('-')
        && !word.contains("://")
        && !word.contains(char::is_whitespace)
        && (word.starts_with(['.', '~']) || word.contains('/'))
}

#[cfg(test)]
mod test;
