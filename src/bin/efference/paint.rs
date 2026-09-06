//! Minimal ANSI for the terminal reports (`cache`, and the `dev` surveys).
//!
//! Its own module because the two groups that use it are gated differently: the
//! cache report is a top-level operator instrument, the spotter survey is a `dev`
//! verb, and a shared helper cannot live in the one that disappears.

// Minimal ANSI so the survey reads at a glance on a tty and stays clean when piped.
pub(crate) const BOLD: &str = "1";

pub(crate) const DIM: &str = "2";

pub(crate) const RED: &str = "31";

pub(crate) const GREEN: &str = "32";

pub(crate) const YELLOW: &str = "33";

#[cfg(feature = "dev")]
pub(crate) const GREEN_BOLD: &str = "1;32";

/// Wrap `s` in an ANSI SGR code, but only when writing to a terminal — piped output stays plain
/// (and grep/`jq`-friendly). The reset always closes the span so padded widths stay correct.
pub(crate) fn paint(s: &str, code: &str, tty: bool) -> String {
    if tty {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
