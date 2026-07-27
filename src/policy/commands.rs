//! Bash-command classification: lexing a command and matching it against prefix grants.

use super::*;

/// Stage 1 + stage 2 for a bash command string. `Clear` only if it is structurally
/// simple *and* its program/args match an allowlist entry.
pub(super) fn classify_bash(role: &Config, command: &str) -> Verdict {
    let Some(tokens) = simple_command_tokens(command) else {
        return Verdict::Gate; // stage 1 rejected (complex, expanded, or unparseable)
    };
    // Stage 2: token-wise prefix match against the command-prefix set. Each key is re-lexed with
    // the *same* lexer as the command, so `"git diff"` compares as `["git", "diff"]`, not by
    // string — and a key that fails to lex (can't happen post-validation) is simply skipped.
    // Re-lexing a handful of keys is free next to parsing the command itself.
    if role.commands.iter().any(|key| {
        simple_command_tokens(key).is_some_and(|key_tokens| tokens.starts_with(&key_tokens))
    }) {
        Verdict::Clear
    } else {
        Verdict::Gate
    }
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
/// - It must not be **already covered** by an existing key — if some current key's tokens are a
///   prefix of the proposed prefix's tokens, any command it would clear already clears, so the
///   promotion is redundant and dropped (keeps the proposal queue free of no-ops; the spotter is
///   blind to the allowlist, so it can't dedup itself).
///
/// Returns the normalized key (the trimmed prefix) to store, or `None` if it fails either check.
pub fn promotable_command(role: &Config, prefix: &str) -> Option<String> {
    let tokens = simple_command_tokens(prefix)?;
    let already_covered = role.commands.iter().any(|key| {
        simple_command_tokens(key).is_some_and(|key_tokens| tokens.starts_with(&key_tokens))
    });
    if already_covered {
        return None;
    }
    Some(prefix.trim().to_string())
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

/// **Stage 1 — structural simplicity.** Parse `command` and return its literal token
/// vector (`[program, args…]`) iff it is exactly one simple command with no operators,
/// redirections, assignments, or expansions, and whose every word is wholly literal.
/// `None` for anything else (including a parse error) — the caller gates it.
///
/// `pub(crate)` because config-load reuses it to validate `[commands]` keys (the soundness
/// requirement that a key lexes to the same token vector the command is checked against).
pub(crate) fn simple_command_tokens(command: &str) -> Option<Vec<String>> {
    use yash_syntax::syntax::{Command, List};

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
    let pipeline = &and_or.first;
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
