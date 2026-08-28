//! Bash-command classification: lexing a command and matching it against prefix grants.

use super::*;

/// Stage 1 + stage 2 for a bash command string. `Clear` only if it is structurally
/// simple *and* the `[commands]` key governing its program/args carries
/// [`CommandStance::Clear`]. Stage 1 runs through [`bash_tokens_in`], so a leading
/// `cd <granted dir> &&` is admitted and the remainder is what stage 2 matches — which means a
/// carve-out governs the same way whether or not the move arrived with a `cd` in front of it.
pub(super) fn classify_bash(role: &Config, command: &str, cwd: &Path) -> Verdict {
    let Some(tokens) = bash_tokens_in(role, command, cwd) else {
        // Stage 1 rejected it. One narrow shape gets a second look before it gates:
        // a redirection or here-doc write to a single literal path, which is a
        // `write` and is governed as one ([`classify_redirect_write`]).
        return classify_redirect_write(role, command, cwd);
    };
    match command_stance(role, &tokens) {
        Some(CommandStance::Clear) => Verdict::Clear,
        // `deny` refuses without a spotter call; `watch`/`gate` spend one to re-decide. Both
        // route to whoever gates in the end, which is why they are distinguishable here and
        // not yet distinguishable in outcome (docs/permissions.md).
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
/// a `clear` command writing to an ungranted path gates. The `tools` set is not consulted:
/// it names structured tools, and the tool being called here is `bash`, whose name grant is
/// its `[commands]` stance. The self-surface stack (docs/permissions.md) needs no special
/// case — it already denies writes under `.efference/`, so a here-doc aimed at a session's
/// own `config.toml` or log fails the path check like anything else there.
///
/// This does not loosen stage 1 generally: every shape outside the list below still gates.
fn classify_redirect_write(role: &Config, command: &str, cwd: &Path) -> Verdict {
    let Some(write) = redirect_write_in(role, command, cwd) else {
        return Verdict::Gate;
    };
    match command_stance(role, &write.tokens) {
        // The operator wrote the answer for this prefix; a redirection cannot soften it.
        Some(CommandStance::Deny) => Verdict::Deny,
        // Cleared as a command — now it must also be cleared as a write.
        Some(CommandStance::Clear) => {
            if path_granted_at(role, &write.target, Perm::Write, cwd) {
                Verdict::Clear
            } else {
                Verdict::Gate
            }
        }
        Some(CommandStance::Watch | CommandStance::Gate) | None => Verdict::Gate,
    }
}

/// The admitted write shapes, and nothing else (docs/permissions.md). Everything here is
/// decided by the same yash parse and the same literal-word inspection stage 1 uses, so
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
///   be wholly literal. An unquoted here-doc expands, and `$(…)` inside one executes — yash
///   surfaces that as a `CommandSubst` unit, so requiring literal content rejects it while
///   still admitting the quoted form and the unquoted-but-literal one;
/// - alongside any number of the fd duplications stage 1 already tolerates ([`is_safe_fd_dup`]);
/// - optionally behind the same single leading `cd <read-granted literal dir> &&`. The write
///   target then resolves against **that** directory, not the process cwd, because that is
///   where the command actually runs.
///
/// Fails closed (gates, exactly as today): pipes, `;`, `||`, backgrounding, more than one
/// file redirection, an input redirection or here-string, a redirection whose target
/// expands or globs, a here-doc containing any expansion or substitution, a negated or
/// compound command, an assignment prefix, and anything that does not parse.
fn redirect_write_in(role: &Config, command: &str, cwd: &Path) -> Option<RedirectWrite> {
    use yash_syntax::syntax::{AndOr, Command, List, RedirBody, RedirOp, TextUnit};

    let list: List = command.parse().ok()?;
    let [item] = list.0.as_slice() else {
        return None;
    };
    if item.async_flag.is_some() {
        return None;
    }
    let and_or = &item.and_or;
    // The `cd` tolerance, and what it does to the target: after `cd sub`, `> out` names
    // `sub/out`. Resolving against the process cwd would check the grant on the wrong file.
    let (pipeline, run_dir) = match and_or.rest.as_slice() {
        [] => (&and_or.first, cwd.to_path_buf()),
        [(AndOr::AndThen, remainder)] => {
            let head = pipeline_tokens(&and_or.first)?;
            let [program, target] = head.as_slice() else {
                return None;
            };
            if program != "cd" || !path_granted(role, target, Perm::Read, cwd) {
                return None;
            }
            (remainder, lexical_normalize(&absolutize(target, cwd)))
        }
        _ => return None,
    };
    if pipeline.negation {
        return None;
    }
    let [command] = pipeline.commands.as_slice() else {
        return None; // no `|`
    };
    let Command::Simple(simple) = &**command else {
        return None;
    };
    if !simple.assigns.is_empty() {
        return None;
    }
    let mut target = None;
    for redir in simple.redirs.iter() {
        if is_safe_fd_dup(redir) {
            continue;
        }
        match &redir.body {
            RedirBody::Normal { operator, operand }
                if matches!(
                    operator,
                    RedirOp::FileOut | RedirOp::FileAppend | RedirOp::FileClobber
                ) && redir.fd.is_none_or(|fd| fd.0 == 1) =>
            {
                if target.is_some() {
                    return None; // exactly one file write, so one path decides
                }
                target = Some(literal_word(operand)?);
            }
            RedirBody::HereDoc(here) if redir.fd.is_none_or(|fd| fd.0 == 0) => {
                // Data, not a file — but only if it is *only* data: an expansion or a
                // command substitution in the content runs.
                let content = here.content.get()?;
                if !content
                    .0
                    .iter()
                    .all(|unit| matches!(unit, TextUnit::Literal(_) | TextUnit::Backslashed(_)))
                {
                    return None;
                }
            }
            _ => return None,
        }
    }
    let target = target?;
    let mut tokens = Vec::with_capacity(simple.words.len());
    for (word, _mode) in &simple.words {
        tokens.push(literal_word(word)?);
    }
    if tokens.is_empty() || tokens.first().is_some_and(|t| t == "cd") {
        return None;
    }
    Some(RedirectWrite {
        target: lexical_normalize(&absolutize(&target, &run_dir)),
        tokens,
    })
}

/// Whether a redirection is a pure file-descriptor **duplication or closure** (`2>&1`, `>&2`,
/// `2>&-`, `<&3`) — the one redirection class stage 1 tolerates. Sound because it never names or
/// touches a file: `>&`/`<&` (yash's [`RedirOp::FdOut`]/[`RedirOp::FdIn`]) only copy or close an
/// already-open descriptor, so there is no filesystem write, read, or exfiltration to gate on.
///
/// The operand must reduce (via [`literal_word`]) to a descriptor: an all-digits fd number or `-`
/// (close). A `>&` whose operand is a filename (`>& out.log`) or an expansion (`2>&$FD`) is *not* a
/// pure dup — it can reach the filesystem — so it returns `false` and the command gates. Here-docs
/// and here-strings are file/data redirections and are rejected here too.
///
/// [`RedirOp::FdOut`]: yash_syntax::syntax::RedirOp::FdOut
/// [`RedirOp::FdIn`]: yash_syntax::syntax::RedirOp::FdIn
pub(super) fn is_safe_fd_dup(redir: &yash_syntax::syntax::Redir) -> bool {
    use yash_syntax::syntax::{RedirBody, RedirOp};
    let RedirBody::Normal { operator, operand } = &redir.body else {
        return false; // here-doc: data redirection, not an fd dup
    };
    if !matches!(operator, RedirOp::FdOut | RedirOp::FdIn) {
        return false; // a file redirection (`>`, `>>`, `<`, `<>`, `>|`, here-string)
    }
    match literal_word(operand) {
        // `-` closes the fd; otherwise the operand must be a bare descriptor number.
        Some(word) => word == "-" || (!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit())),
        None => false, // expanded/globbed operand → not a decidable fd
    }
}

/// **Stage 1 — structural simplicity, for a bash *tool call*.** [`simple_command_tokens`], plus
/// the one tolerated operator: a single leading `cd <dir> &&`, whose remainder becomes the token
/// vector stage 2 matches and prefix-promotion probes (docs/permissions.md).
///
/// The admitted shape is narrow, and everything outside it gates exactly as before:
///
/// - the `cd` segment is exactly two words, `cd` and a **literal** target — the same word-unit
///   inspection stage 1 already performs ([`literal_word`]), so an expansion, a glob, or a tilde in
///   the target gates;
/// - the target, resolved against `cwd` the way every other grant check resolves a path
///   ([`path_granted`]), must sit under a `[paths]` grant carrying **read** — `cd` mutates nothing,
///   so read is the permission the move actually needs, and a write grant satisfies it too;
/// - the operator is `&&` only (`;`, `||`, `|` gate), there is exactly **one** of it, and the
///   remainder is itself a simple command that is not a second `cd`.
///
/// The reason for the tolerance is the same as the pure-fd-dup one above: a habitual prefix that
/// models emit on nearly every call knocks a blanket `[commands]` grant off the fast path, and
/// every such call pays a spotter round-trip. Recording the **remainder** (not the whole string) is
/// what makes the promotion generalize across run directories.
pub(crate) fn bash_tokens_in(role: &Config, command: &str, cwd: &Path) -> Option<Vec<String>> {
    use yash_syntax::syntax::{AndOr, List};

    let list: List = command.parse().ok()?;
    // Exactly one item (no `;`-separated list), not backgrounded (`&`).
    let [item] = list.0.as_slice() else {
        return None;
    };
    if item.async_flag.is_some() {
        return None;
    }
    let and_or = &item.and_or;
    match and_or.rest.as_slice() {
        [] => pipeline_tokens(&and_or.first),
        [(AndOr::AndThen, remainder)] => {
            // A leading `cd <literal dir>` into a read-granted directory, and nothing else.
            let head = pipeline_tokens(&and_or.first)?;
            let [program, target] = head.as_slice() else {
                return None;
            };
            if program != "cd" || !path_granted(role, target, Perm::Read, cwd) {
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

/// **Stage 1 — structural simplicity.** Parse `command` and return its literal token
/// vector (`[program, args…]`) iff it is exactly one simple command with no operators,
/// redirections, assignments, or expansions, and whose every word is wholly literal.
/// `None` for anything else (including a parse error) — the caller gates it.
///
/// `pub(crate)` because config-load reuses it to validate `[commands]` keys (the soundness
/// requirement that a key lexes to the same token vector the command is checked against). That
/// use is why this stays strict about operators: a `[commands]` key is never a `cd …` compound.
pub(crate) fn simple_command_tokens(command: &str) -> Option<Vec<String>> {
    use yash_syntax::syntax::List;

    let list: List = command.parse().ok()?;
    // Exactly one item (no `;`-separated list), not backgrounded (`&`).
    let [item] = list.0.as_slice() else {
        return None;
    };
    if item.async_flag.is_some() {
        return None;
    }
    let and_or = &item.and_or;
    if !and_or.rest.is_empty() {
        return None; // no `&&` / `||`
    }
    pipeline_tokens(&and_or.first)
}

/// The single-simple-command core of stage 1: one pipeline reduced to its literal token vector, or
/// `None` if it is negated, piped, compound, assignment-prefixed, file-redirected, or holds a word
/// bash would expand.
fn pipeline_tokens(pipeline: &yash_syntax::syntax::Pipeline) -> Option<Vec<String>> {
    use yash_syntax::syntax::Command;

    if pipeline.negation {
        return None; // no leading `!`
    }
    let [command] = pipeline.commands.as_slice() else {
        return None; // no `|` — exactly one command
    };
    let Command::Simple(simple) = &**command else {
        return None; // not a compound command / function definition
    };
    if !simple.assigns.is_empty() {
        return None; // no `FOO=bar cmd` env-assignment prefix
    }
    // Redirections gate — *except* a file-descriptor duplication/closure (`2>&1`, `>&2`, `2>&-`,
    // `<&3`), which touches no filesystem: it only rewires the process's own descriptors among
    // already-open fds. That's sound under the error-not-deception model, and it's the habitual
    // `cmd 2>&1` the model appends; without this a trailing `2>&1` knocks a blanket-allowlisted
    // program off the fast path (docs/permissions.md). A file redirection, here-doc, or expanded
    // operand still gates via [`is_safe_fd_dup`].
    if !simple.redirs.iter().all(is_safe_fd_dup) {
        return None;
    }
    let mut tokens = Vec::with_capacity(simple.words.len());
    // `words` pairs each word with its expansion mode; we only need the word.
    for (word, _mode) in &simple.words {
        tokens.push(literal_word(word)?);
    }
    // Defensive: a simple command with no words shouldn't reach here, but never return
    // an empty token vector (the caller assumes a program word exists).
    if tokens.is_empty() {
        return None;
    }
    Some(tokens)
}

/// Reduce a [`Word`] to its literal string, or `None` if any unit would be **expanded**
/// or **globbed** by bash at runtime. yash parses without expanding, so we inspect the
/// units ourselves:
///
/// - single-quoted text is literal (a `|` or `*` inside `'…'` is just text — this is why
///   a regex would be wrong and a real lexer is needed);
/// - double-quoted text is literal only if it contains no `$…`/`` `…` `` expansion;
/// - unquoted literal chars are fine *except* the glob/brace metacharacters `* ? [ {`
///   (bash globs them anywhere) and a leading `~` (bash tilde-expands only at word
///   start, so `HEAD~1` is fine but `~/x` is not);
/// - anything else (unquoted parameter/command substitution, a tilde unit, `$'…'`) →
///   `None`.
///
/// [`Word`]: yash_syntax::syntax::Word
pub(super) fn literal_word(word: &yash_syntax::syntax::Word) -> Option<String> {
    use yash_syntax::syntax::{TextUnit, WordUnit};

    let mut out = String::new();
    for (index, unit) in word.units.iter().enumerate() {
        match unit {
            WordUnit::SingleQuote(text) => out.push_str(text),
            WordUnit::DoubleQuote(text) => {
                for tu in &text.0 {
                    match tu {
                        TextUnit::Literal(c) | TextUnit::Backslashed(c) => out.push(*c),
                        _ => return None, // $param / $(...) / `...` / $((...)) inside "…"
                    }
                }
            }
            WordUnit::Unquoted(TextUnit::Literal(c)) => {
                if matches!(c, '*' | '?' | '[' | '{') {
                    return None; // unquoted glob / brace-expansion metacharacter
                }
                if *c == '~' && index == 0 {
                    return None; // leading unquoted tilde → tilde expansion
                }
                out.push(*c);
            }
            WordUnit::Unquoted(TextUnit::Backslashed(c)) => out.push(*c), // escaped literal
            _ => return None, // unquoted expansion, tilde unit, $'…', etc.
        }
    }
    Some(out)
}
