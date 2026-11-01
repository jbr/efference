//! Terminal formatting removal (docs/process-monitor.md).
//!
//! A process that believes it writes to a terminal colours its output, rewrites
//! lines with carriage returns, and wraps text in hyperlinks. The capture keeps
//! those bytes; the text the agent reads, and the triage that reads it, goes
//! through [`strip_formatting`] first. So does text someone else supplied that reaches
//! the operator's terminal, such as a device's label. The aim is readable text, not an
//! emulated screen: nothing is positioned, and an overwritten line keeps only its
//! last state.

/// `s` without terminal formatting.
///
/// - CSI sequences (`ESC [ … final`, colour and cursor movement alike) are removed.
/// - OSC strings (`ESC ] … BEL` or `ESC ] … ESC \`) are removed, so an OSC 8
///   hyperlink keeps its visible text and drops the target. The device-control
///   strings (`ESC P`, `ESC X`, `ESC ^`, `ESC _`) are removed the same way. A string
///   with no terminator ends at the end of its line.
/// - Any other `ESC` sequence (a two-byte `ESC x`, or a charset selection such as
///   `ESC ( B`) is removed.
/// - Within a line, a carriage return starts a redraw: only the last non-empty
///   segment is kept. A `\r\n` ending is a line ending and is preserved; a `\r` that
///   ends the text with nothing after it does not erase the line.
///
/// Text without escapes or carriage returns comes back byte-identical.
pub fn strip_formatting(s: &str) -> String {
    if !s.contains(['\x1b', '\u{9b}', '\r']) {
        return s.to_string();
    }
    let unescaped = strip_escapes(s);
    let mut out = String::with_capacity(unescaped.len());
    for line in unescaped.split_inclusive('\n') {
        let (body, ending) = match line.strip_suffix("\r\n") {
            Some(body) => (body, "\r\n"),
            None => match line.strip_suffix('\n') {
                Some(body) => (body, "\n"),
                None => (line, ""),
            },
        };
        out.push_str(last_state(body));
        out.push_str(ending);
    }
    out
}

/// The text a line shows after its redraws: the last non-empty `\r`-separated segment.
fn last_state(body: &str) -> &str {
    body.rsplit('\r').find(|s| !s.is_empty()).unwrap_or("")
}

fn strip_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{9b}' => skip_csi(&mut chars),
            '\x1b' => match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    skip_csi(&mut chars);
                }
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    chars.next();
                    skip_string(&mut chars);
                }
                Some(' '..='/') => {
                    // Intermediate bytes, then the one final byte.
                    while chars.next_if(|c| matches!(c, ' '..='/')).is_some() {}
                    chars.next_if(|c| matches!(c, '0'..='~'));
                }
                Some('0'..='~') => {
                    chars.next();
                }
                // A bare ESC before a line break, a control byte, or the end.
                _ => {}
            },
            c => out.push(c),
        }
    }
    out
}

/// Consume the rest of a CSI sequence: parameter bytes, intermediate bytes, one final byte.
fn skip_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while chars.next_if(|c| matches!(c, '0'..='?')).is_some() {}
    while chars.next_if(|c| matches!(c, ' '..='/')).is_some() {}
    chars.next_if(|c| matches!(c, '@'..='~'));
}

/// Consume a string-terminated sequence's payload and its terminator (`BEL` or
/// `ESC \`), stopping before a line break if the terminator never comes.
fn skip_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(c) = chars.next_if(|c| *c != '\n') {
        match c {
            '\x07' => return,
            '\x1b' => {
                chars.next_if(|c| *c == '\\');
                return;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod test;
