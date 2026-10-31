//! Bash-command classification: lexing a command and matching it against prefix grants.

use super::*;

/// Stage 1 + stage 2 for a bash command string. `Clear` only if it is structurally
/// simple *and* the `[commands]` key governing its program/args carries
/// [`CommandStance::Clear`]. Stage 1 runs through [`bash_tokens_in`], so a leading
/// `cd <granted dir> &&` is admitted and the remainder is what stage 2 matches — which means a
/// carve-out governs the same way whether or not the move arrived with a `cd` in front of it.
pub(super) fn classify_bash(role: &Config, command: &str, dirs: &Directories) -> Verdict {
    let Some(tokens) = bash_tokens_in(role, command, dirs) else {
        // Stage 1 rejected it. One narrow shape gets a second look before it gates:
        // a redirection or here-doc write to a single literal path, which is a
        // `write` and is governed as one ([`classify_redirect_write`]).
        return classify_redirect_write(role, command, dirs);
    };
    match command_stance(role, &tokens) {
        Some(CommandStance::Clear) => Verdict::Clear,
        // `deny` refuses without a spotter call; `watch`/`gate` spend one to re-decide. Both
        // route to whoever gates in the end, which is why they are distinguishable here and
        // not distinguishable in outcome (docs/permissions.md).
        Some(CommandStance::Deny) => Verdict::Deny,
        Some(CommandStance::Watch | CommandStance::Gate) | None => Verdict::Gate,
    }
}

/// The stance governing a lexed command, or `None` if no `[commands]` key covers it.
///
/// **Stage 2 — the prefix match, resolved longest-prefix-wins** (docs/permissions.md), the same
/// way `[paths]` resolves a filesystem grant: among the keys whose tokens are a prefix of the
/// command's, the *most specific* (longest) one governs, so `git = "clear"` plus
/// `"git push" = "deny"` clears `git log` and denies `git push origin main`. Trust-broad-then-
/// carve-exceptions is the shape this exists for; without it a broad grant could only be
/// narrowed by deleting it and re-listing every safe sub-verb.
///
/// Each key is re-lexed with the *same* lexer as the command, so `"git diff"` compares as
/// `["git", "diff"]`, not by string — and a key that fails to lex (can't happen
/// post-validation) is simply skipped. Re-lexing a handful of keys is free next to parsing the
/// command itself. Ties at equal token depth go to the more restrictive stance
/// ([`CommandStance`]'s `Ord`).
pub(super) fn command_stance(role: &Config, tokens: &[String]) -> Option<CommandStance> {
    role.commands
        .iter()
        .filter_map(|(key, stance)| {
            let key_tokens = simple_command_tokens(key)?;
            tokens
                .starts_with(&key_tokens)
                .then_some((key_tokens.len(), *stance))
        })
        .max()
        .map(|(_, stance)| stance)
}

/// Validate a spotter-proposed command prefix for **promotion** into the `[commands]` allowlist
/// (docs/permissions.md, spotter.md). The spotter, fresh-context, judges a
/// program-prefix *standing-safe* (reversible/in-scope regardless of args) and suggests adding it;
/// this is the soundness gate before that suggestion becomes a `Proposed(Command)`:
///
/// - It must **lex to a simple-command token vector** with the fast-path's own lexer — the same
///   requirement config-load enforces on `[commands]` keys, so the stored key and any command it's
///   matched against agree token-for-token. A prefix that doesn't lex (`"git | rm"`, an expansion)
///   is rejected, never queued. This is what holds the **program-prefix-granularity** invariant
///   (safety is discontinuous in the command string, so only program-prefixes are soundly
///   memoizable, never exact commands).
/// - It must not be **already governed** by an existing key — if some current key's tokens are a
///   prefix of the proposed prefix's tokens, that key already decides every command the promotion
///   would address, so the promotion is dropped (keeps the proposal queue free of no-ops; the
///   spotter is blind to the allowlist, so it can't dedup itself). This covers both directions of
///   redundancy now that a key can carry any stance: under a governing `clear` the promotion is a
///   no-op, and under a governing `watch`/`gate`/`deny` it would punch a hole in a carve-out the
///   operator wrote — a change only the operator should make, in the file.
///
/// Returns the normalized key (the trimmed prefix) to store, or `None` if it fails either check.
pub fn promotable_command(role: &Config, prefix: &str) -> Option<String> {
    let tokens = simple_command_tokens(prefix)?;
    if command_stance(role, &tokens).is_some() {
        return None;
    }
    Some(prefix.trim().to_string())
}

/// A bash command that is a **write to one literal path** — the redirection carve-out
/// (docs/permissions.md). `tokens` is the command as stage 2 would see it with the
/// redirection removed; `target` is the file it writes, already resolved against the
/// directory the command runs in.
struct RedirectWrite {
    tokens: Vec<String>,
    target: PathBuf,
}

/// **The redirection carve-out**: a strictly-parseable redirection or here-doc write to a
/// single literal path is a `write`, governed by the `[paths]` write grants
/// (docs/permissions.md).
///
/// Stage 1 rejects file redirections outright, so before this a `cat > notes.md <<EOF`
/// always cost a spotter round-trip — and under gating friction the heredoc *is* the shape
/// a model reaches for when the structured `write` doesn't go through, which turns a
/// reviewable diff into an un-reviewable whole-file rewrite. Governing it as the write it
/// is makes the granted case cheap and the ungranted case gate the way the structured
/// `write` gates, so routing around the tool changes nothing about the answer.
///
/// **Two authorities govern, and the more restrictive one wins.** The command still has to
/// clear stage 2 on its own (`cat` must carry a `clear` stance), *and* the target still has
/// to be write-granted by `[paths]`. A `[commands]` `deny` refuses regardless of the path;
/// a `clear` command writing to an ungranted path gates. The `[tools]` table is not consulted:
/// it names structured tools, and the tool being called here is `bash`, whose name grant is
/// its `[commands]` stance. The self-surface stack (docs/permissions.md) needs no special
/// case — it already denies writes under `.efference/`, so a here-doc aimed at a session's
/// own `config.toml` or log fails the path check like anything else there.
///
/// This does not loosen stage 1 generally: every shape outside the list below still gates.
fn classify_redirect_write(role: &Config, command: &str, dirs: &Directories) -> Verdict {
    let Some(write) = redirect_write_in(role, command, dirs) else {
        return Verdict::Gate;
    };
    match command_stance(role, &write.tokens) {
        // The operator wrote the answer for this prefix; a redirection cannot soften it.
        Some(CommandStance::Deny) => Verdict::Deny,
        // Cleared as a command — now it must also be cleared as a write.
        Some(CommandStance::Clear) => {
            if path_granted_at(role, &dirs.judged(&write.target), Perm::Write, dirs) {
                Verdict::Clear
            } else {
                Verdict::Gate
            }
        }
        Some(CommandStance::Watch | CommandStance::Gate) | None => Verdict::Gate,
    }
}

/// The admitted write shapes, and nothing else (docs/permissions.md). Everything here is
/// decided by the same brush parse and the same literal-word inspection stage 1 uses, so
/// "strictly parseable" means the same thing it means there.
///
/// Admitted:
///
/// - **one simple command** whose every word is literal (no operators, no expansions, no
///   assignment prefix, no glob) — the stage 1 core;
/// - carrying **exactly one** file-write redirection: `>` (truncate), `>>` (append), or
///   `>|` (truncate past `noclobber`). All three are writes, and a write grant covers a
///   write; the distinction between them is not one `[paths]` can express. Its fd must be
///   unspecified or `1` — `2> file` is a write too, but admitting only the shape models
///   actually emit keeps the surface small.
/// - whose **operand is a literal path** ([`literal_word`]) — an expansion or a glob in the
///   target means the destination isn't decidable here, so it gates;
/// - optionally a **here-doc on stdin** (`<<EOF`, `<<'EOF'`, `<<-EOF`), whose *content* must
///   be wholly literal. An unquoted here-doc expands, and `$(…)` inside one executes —
///   requiring literal content ([`literal_heredoc`]) rejects it while still admitting the
///   quoted form and the unquoted-but-literal one;
/// - alongside any number of the fd duplications stage 1 already tolerates ([`is_safe_fd_dup`]);
/// - optionally behind the same single leading `cd <read-granted literal dir> &&`. The write
///   target then resolves against **that** directory, not the working directory, because that is
///   where the command actually runs.
///
/// Fails closed (gates, exactly as today): pipes, `;`, `||`, backgrounding, more than one
/// file redirection, an input redirection or here-string, a redirection whose target
/// expands or globs, a here-doc containing any expansion or substitution, a negated or
/// compound command, an assignment prefix, and anything that does not parse.
fn redirect_write_in(role: &Config, command: &str, dirs: &Directories) -> Option<RedirectWrite> {
    use brush_parser::ast::{AndOr, Command, IoFileRedirectKind, IoFileRedirectTarget, IoRedirect};

    let and_or = single_and_or_list(command)?;
    // The `cd` tolerance, and what it does to the target: after `cd sub`, `> out` names
    // `sub/out`. Resolving against the working directory would check the grant on the wrong file.
    let (pipeline, run_dir) = match and_or.additional.as_slice() {
        [] => (&and_or.first, dirs.working().to_path_buf()),
        [AndOr::And(remainder)] => {
            let head = pipeline_tokens(&and_or.first)?;
            let [program, target] = head.as_slice() else {
                return None;
            };
            if program != "cd" || !path_granted(role, target, Perm::Read, dirs) {
                return None;
            }
            (
                remainder,
                lexical_normalize(&absolutize(target, dirs.working())),
            )
        }
        _ => return None,
    };
    if pipeline.bang || pipeline.timed.is_some() {
        return None;
    }
    let [command] = pipeline.seq.as_slice() else {
        return None; // no `|`
    };
    let Command::Simple(simple) = command else {
        return None;
    };
    let mut target = None;
    let tokens = simple_tokens(simple, |redir| {
        if is_safe_fd_dup(redir) {
            return true;
        }
        match redir {
            IoRedirect::File(fd, kind, IoFileRedirectTarget::Filename(operand))
                if matches!(
                    kind,
                    IoFileRedirectKind::Write
                        | IoFileRedirectKind::Append
                        | IoFileRedirectKind::Clobber
                ) && fd.is_none_or(|fd| fd == 1) =>
            {
                if target.is_some() {
                    return false; // exactly one file write, so one path decides
                }
                match literal_word(operand) {
                    Some(path) => {
                        target = Some(path);
                        true
                    }
                    None => false,
                }
            }
            IoRedirect::HereDocument(fd, here) if fd.is_none_or(|fd| fd == 0) => {
                // Data, not a file — but only if it is *only* data: an expansion or a
                // command substitution in the content runs. A quoted delimiter
                // (`<<'EOF'`) makes the body literal by construction.
                !here.requires_expansion || literal_heredoc(&here.doc.value)
            }
            _ => false,
        }
    })?;
    let target = target?;
    if tokens.first().is_none_or(|t| t == "cd") {
        return None;
    }
    Some(RedirectWrite {
        target: lexical_normalize(&absolutize(&target, &run_dir)),
        tokens,
    })
}

/// Whether an *unquoted* here-doc body is wholly literal: no parameter expansion, no
/// command or arithmetic substitution (`$(…)` in an unquoted here-doc *executes*). An
/// escape counts as data — inside a here-doc a backslash only quotes `$`, `` ` ``, `\`,
/// and newline.
fn literal_heredoc(content: &str) -> bool {
    use brush_parser::word::WordPiece;
    brush_parser::word::parse_heredoc(content, &parser_options()).is_ok_and(|pieces| {
        pieces.iter().all(|piece| {
            matches!(
                piece.piece,
                WordPiece::Text(_) | WordPiece::EscapeSequence(_)
            )
        })
    })
}

/// Whether a redirection is a pure file-descriptor **duplication or closure** (`2>&1`, `>&2`,
/// `2>&-`, `<&3`) — the one redirection class stage 1 tolerates. Sound because it never names or
/// touches a file: `>&`/`<&` ([`IoFileRedirectKind::DuplicateOutput`]/[`DuplicateInput`]) only
/// copy or close an already-open descriptor, so there is no filesystem write, read, or
/// exfiltration to gate on.
///
/// The operand must be a descriptor: a bare fd number, or a word that reduces (via
/// [`literal_word`]) to an all-digits number or `-` (close). A `>&` whose operand is a filename
/// (`>& out.log`) or an expansion (`2>&$FD`) is *not* a pure dup — it can reach the filesystem —
/// so it returns `false` and the command gates. Here-docs, here-strings, and `&>` are file/data
/// redirections and are rejected here too.
///
/// [`IoFileRedirectKind::DuplicateOutput`]: brush_parser::ast::IoFileRedirectKind::DuplicateOutput
/// [`DuplicateInput`]: brush_parser::ast::IoFileRedirectKind::DuplicateInput
pub(super) fn is_safe_fd_dup(redir: &brush_parser::ast::IoRedirect) -> bool {
    use brush_parser::ast::{IoFileRedirectKind, IoFileRedirectTarget, IoRedirect};
    let IoRedirect::File(
        _,
        IoFileRedirectKind::DuplicateInput | IoFileRedirectKind::DuplicateOutput,
        target,
    ) = redir
    else {
        // A file redirection (`>`, `>>`, `<`, `<>`, `>|`, `&>`), here-doc, or here-string.
        return false;
    };
    match target {
        IoFileRedirectTarget::Fd(_) => true,
        // `-` closes the fd; otherwise the operand must be a bare descriptor number.
        IoFileRedirectTarget::Duplicate(word) => match literal_word(word) {
            Some(word) => {
                word == "-" || (!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
            }
            None => false, // expanded/globbed operand → not a decidable fd
        },
        // A filename (`>& out.log`) or process substitution reaches the filesystem.
        _ => false,
    }
}

/// **Stage 1 — structural simplicity, for a bash *tool call*.** [`simple_command_tokens`], plus
/// the one tolerated operator: a single leading `cd <dir> &&`, whose remainder becomes the token
/// vector stage 2 matches and prefix-promotion probes (docs/permissions.md).
///
/// The admitted shape is narrow, and everything outside it gates:
///
/// - the `cd` segment is exactly two words, `cd` and a **literal** target — the same word-unit
///   inspection stage 1 already performs ([`literal_word`]), so an expansion, a glob, or a tilde in
///   the target gates;
/// - the target, resolved by [`Directories::target`] the way every other grant check resolves a path
///   ([`path_granted`]), must sit under a `[paths]` grant carrying **read** — `cd` mutates nothing,
///   so read is the permission the move actually needs, and a write grant satisfies it too;
/// - the operator is `&&` only (`;`, `||`, `|` gate), there is exactly **one** of it, and the
///   remainder is itself a simple command that is not a second `cd`.
///
/// The reason for the tolerance is the same as the pure-fd-dup one above: a habitual prefix that
/// models emit on nearly every call knocks a blanket `[commands]` grant off the fast path, and
/// every such call pays a spotter round-trip. Recording the **remainder** (not the whole string) is
/// what makes the promotion generalize across run directories.
pub(crate) fn bash_tokens_in(
    role: &Config,
    command: &str,
    dirs: &Directories,
) -> Option<Vec<String>> {
    use brush_parser::ast::AndOr;

    let and_or = single_and_or_list(command)?;
    match and_or.additional.as_slice() {
        [] => pipeline_tokens(&and_or.first),
        [AndOr::And(remainder)] => {
            // A leading `cd <literal dir>` into a read-granted directory, and nothing else.
            let head = pipeline_tokens(&and_or.first)?;
            let [program, target] = head.as_slice() else {
                return None;
            };
            if program != "cd" || !path_granted(role, target, Perm::Read, dirs) {
                return None;
            }
            let tokens = pipeline_tokens(remainder)?;
            if tokens.first().is_none_or(|t| t == "cd") {
                return None; // one leading `cd` only, and never an empty remainder
            }
            Some(tokens)
        }
        _ => None, // `||`, or more than one operator
    }
}

/// A leading `cd` into the session's own working directory changes nothing about
/// what the rest of the command touches, so the spotter judges the rest
/// (docs/permissions.md): agents prefix their own cwd habitually, and a reviewer
/// asked to resolve `cd /abs/path` against a working-directory line reads the move
/// as leaving it. Returns the remainder as written when `command` is `cd <dir> &&
/// rest` with `<dir>` a literal that resolves, lexically, to the working directory; `None` for
/// anything else, including a `cd` somewhere else, which the reviewer must see.
pub(crate) fn without_cd_into<'a>(command: &'a str, dirs: &Directories) -> Option<&'a str> {
    let (head, rest) = command.split_once("&&")?;
    let tokens = simple_command_tokens(head)?;
    let [program, target] = tokens.as_slice() else {
        return None;
    };
    if program != "cd" {
        return None;
    }
    let lexical = lexical_normalize;
    let cwd = dirs.working();
    if lexical(&cwd.join(target)) != lexical(cwd) {
        return None;
    }
    let rest = rest.trim_start();
    (!rest.is_empty()).then_some(rest)
}

/// **Stage 1 — structural simplicity.** Parse `command` and return its literal token
/// vector (`[program, args…]`) iff it is exactly one simple command with no operators,
/// redirections, assignments, or expansions, and whose every word is wholly literal.
/// `None` for anything else (including a parse error) — the caller gates it.
///
/// `pub(crate)` because config-load reuses it to validate `[commands]` keys (the soundness
/// requirement that a key lexes to the same token vector the command is checked against). That
/// use is why this stays strict about operators: a `[commands]` key is never a `cd …` compound.
pub(crate) fn simple_command_tokens(command: &str) -> Option<Vec<String>> {
    let and_or = single_and_or_list(command)?;
    if !and_or.additional.is_empty() {
        return None; // no `&&` / `||`
    }
    pipeline_tokens(&and_or.first)
}

/// One parser configuration for every parse in this module: bash dialect (no POSIX/`sh`
/// compatibility modes — the command will run under bash, so bash is what to parse),
/// extended globbing off (bash's own default, so `@(…)` fails to parse and gates), and
/// tilde recognition at word start only — the position where bash expands a tilde in an
/// argument, so `~/x` gates while `HEAD~1` and `a:~/b` stay literal.
pub(super) fn parser_options() -> brush_parser::ParserOptions {
    brush_parser::ParserOptions {
        enable_extended_globbing: false,
        posix_mode: false,
        sh_mode: false,
        tilde_expansion_at_word_start: true,
        tilde_expansion_after_colon: false,
        ..Default::default()
    }
}

/// Parse `command` and return its single and-or list, or `None` if it is empty, holds
/// more than one statement (`;`, a newline between commands), is backgrounded (`&`), or
/// does not parse.
fn single_and_or_list(command: &str) -> Option<brush_parser::ast::AndOrList> {
    use brush_parser::ast::{CompoundListItem, SeparatorOperator};

    let mut parser = brush_parser::Parser::new(command.as_bytes(), &parser_options());
    let program = parser.parse_program().ok()?;
    let [list] = <[_; 1]>::try_from(program.complete_commands).ok()?;
    let [CompoundListItem(and_or, separator)] = <[_; 1]>::try_from(list.0).ok()?;
    match separator {
        SeparatorOperator::Async => None, // backgrounded
        SeparatorOperator::Sequence => Some(and_or),
    }
}

/// The single-simple-command core of stage 1: one pipeline reduced to its literal token vector, or
/// `None` if it is negated, timed, piped, compound, assignment-prefixed, file-redirected, or holds
/// a word bash would expand.
fn pipeline_tokens(pipeline: &brush_parser::ast::Pipeline) -> Option<Vec<String>> {
    use brush_parser::ast::Command;

    if pipeline.bang || pipeline.timed.is_some() {
        return None; // no leading `!` or `time`
    }
    let [command] = pipeline.seq.as_slice() else {
        return None; // no `|` — exactly one command
    };
    let Command::Simple(simple) = command else {
        return None; // not a compound command / function definition
    };
    // Redirections gate — *except* a file-descriptor duplication/closure (`2>&1`, `>&2`, `2>&-`,
    // `<&3`), which touches no filesystem: it only rewires the process's own descriptors among
    // already-open fds. That's sound under the error-not-deception model, and it's the habitual
    // `cmd 2>&1` the model appends; without this a trailing `2>&1` knocks a blanket-allowlisted
    // program off the fast path (docs/permissions.md). A file redirection, here-doc, or expanded
    // operand still gates via [`is_safe_fd_dup`].
    simple_tokens(simple, is_safe_fd_dup)
}

/// A simple command reduced to its literal token vector — the program word plus each
/// argument, every one through [`literal_word`] — with each redirection referred to
/// `redirect` (`false` rejects the whole command). Rejects an env-assignment prefix
/// (`FOO=bar cmd` mutates the command's environment) and a process substitution. An
/// assignment-*shaped* argument (`make FOO=1`) is a plain token: after the program word,
/// bash passes it through like any other.
fn simple_tokens(
    simple: &brush_parser::ast::SimpleCommand,
    mut redirect: impl FnMut(&brush_parser::ast::IoRedirect) -> bool,
) -> Option<Vec<String>> {
    use brush_parser::ast::CommandPrefixOrSuffixItem;

    for item in simple.prefix.iter().flat_map(|prefix| &prefix.0) {
        match item {
            CommandPrefixOrSuffixItem::IoRedirect(redir) if redirect(redir) => {}
            _ => return None, // an assignment prefix, or a rejected redirection
        }
    }
    let mut tokens = vec![literal_word(simple.word_or_name.as_ref()?)?];
    for item in simple.suffix.iter().flat_map(|suffix| &suffix.0) {
        match item {
            CommandPrefixOrSuffixItem::Word(word) => tokens.push(literal_word(word)?),
            CommandPrefixOrSuffixItem::AssignmentWord(_, word) => tokens.push(literal_word(word)?),
            CommandPrefixOrSuffixItem::IoRedirect(redir) if redirect(redir) => {}
            _ => return None,
        }
    }
    Some(tokens)
}

/// Reduce a [`Word`] to its literal string, or `None` if any piece would be **expanded**
/// or **globbed** by bash at runtime. brush parses without expanding, so we inspect the
/// word pieces ourselves:
///
/// - single-quoted text is literal (a `|` or `*` inside `'…'` is just text — this is why
///   a regex would be wrong and a real lexer is needed);
/// - double-quoted text is literal only if every piece inside it is;
/// - unquoted text is literal *except* the glob/brace metacharacters `* ? [ {` (bash
///   globs or brace-expands them anywhere in a word);
/// - an escape (`\x`) contributes the escaped character;
/// - anything else — a tilde the parser recognized as expandable (leading only, per
///   [`parser_options`], so `HEAD~1` stays literal), a parameter/command/arithmetic
///   substitution, `$'…'` — → `None`.
///
/// [`Word`]: brush_parser::ast::Word
pub(super) fn literal_word(word: &brush_parser::ast::Word) -> Option<String> {
    let pieces = brush_parser::word::parse(&word.value, &parser_options()).ok()?;
    literal_pieces(&pieces, false, false)
}

/// [`literal_word`], except that unquoted `*`, `?`, and `[` pass through as text:
/// the word is a pathname pattern that expands to paths under its fixed prefix
/// and nowhere else. Brace expansion, tildes, and every other expansion still
/// return `None`.
pub(super) fn pattern_word(word: &brush_parser::ast::Word) -> Option<String> {
    let pieces = brush_parser::word::parse(&word.value, &parser_options()).ok()?;
    literal_pieces(&pieces, false, true)
}

/// The piece walk under [`literal_word`]; `quoted` is true inside double quotes, where
/// the glob metacharacters are just text.
fn literal_pieces(
    pieces: &[brush_parser::word::WordPieceWithSource],
    quoted: bool,
    pattern: bool,
) -> Option<String> {
    use brush_parser::word::WordPiece;

    let mut out = String::new();
    for piece in pieces {
        match &piece.piece {
            WordPiece::Text(text) => {
                let expands: &[char] = if pattern {
                    &['{']
                } else {
                    &['*', '?', '[', '{']
                };
                if !quoted && text.contains(expands) {
                    return None; // unquoted glob / brace-expansion metacharacter
                }
                out.push_str(text);
            }
            WordPiece::SingleQuotedText(text) => out.push_str(text),
            // The sequence is the backslash plus the character it escapes.
            WordPiece::EscapeSequence(sequence) => out.push_str(sequence.strip_prefix('\\')?),
            WordPiece::DoubleQuotedSequence(inner) => {
                out.push_str(&literal_pieces(inner, true, pattern)?);
            }
            _ => return None, // expansion, substitution, tilde, $'…'
        }
    }
    Some(out)
}
