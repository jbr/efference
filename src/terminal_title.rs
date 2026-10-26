//! The terminal title: the line a terminal shows on a window's tab, which the TUI
//! sets to the attached session and a foreground host sets to its session count
//! (docs/tui.md, docs/operating.md). Distinct from a session's title
//! (docs/session-title.md), which the TUI's terminal title quotes.
//!
//! A program sets it by writing OSC 0 to its terminal. A writer [`save`]s the
//! title it found on the terminal's title stack (XTWINOPS 22) before showing its
//! own, and [`restore`]s it (XTWINOPS 23) when it stops, so leaving hands back the
//! shell's title instead of a stale one. A terminal without the stack ignores both
//! sequences; a shell that titles its prompt rewrites the title at the next one.

use std::io::{self, Write};

#[cfg(test)]
mod test;

/// XTWINOPS 22;0: push the icon and window titles onto the title stack.
const SAVE: &[u8] = b"\x1b[22;0t";
/// XTWINOPS 23;0: pop them back.
const RESTORE: &[u8] = b"\x1b[23;0t";

/// Save the terminal title in place, to be put back by [`restore`].
pub fn save(out: &mut impl Write) -> io::Result<()> {
    out.write_all(SAVE)?;
    out.flush()
}

/// Put back the terminal title the matching [`save`] found.
pub fn restore(out: &mut impl Write) -> io::Result<()> {
    out.write_all(RESTORE)?;
    out.flush()
}

/// The TUI's terminal title for an attached session: its title, then its id, the
/// title leading because a tab truncates from the right.
pub fn for_session(id: &str, title: Option<&str>) -> String {
    match title {
        Some(title) => format!("{title} · {id}"),
        None => format!("efference · {id}"),
    }
}

/// A foreground host's terminal title.
pub fn for_host(sessions: usize) -> String {
    match sessions {
        1 => "efference · 1 session".to_string(),
        n => format!("efference · {n} sessions"),
    }
}

/// The terminal title last written, so an unchanged one is not written again.
#[derive(Debug, Default)]
pub struct TerminalTitle {
    shown: Option<String>,
}

impl TerminalTitle {
    /// Show `text` as the terminal title, unless it already is.
    ///
    /// Control characters are dropped first: a session title is model-written
    /// (docs/session-title.md), and an ESC or BEL inside it would end the OSC
    /// early and have the terminal interpret the rest as control sequences.
    pub fn show(&mut self, out: &mut impl Write, text: &str) -> io::Result<()> {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if self.shown.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        write!(out, "\x1b]0;{text}\x07")?;
        out.flush()?;
        self.shown = Some(text);
        Ok(())
    }
}
