//! The [`Prompter`] for a real terminal (docs/operating.md): inline prompts on
//! stdout — or on `/dev/tty` when stdout is a pipe, as under `op run`, which
//! pipes it to mask secrets — raw mode only while a prompt is open, and every path out of a prompt
//! restoring the terminal — including cancellation, which is a return value
//! here rather than a signal.
//!
//! A menu redraws in place, and an answered prompt collapses to one line, so the
//! transcript that is left on screen reads as a record of what was chosen.
//! Secret entry echoes a fixed mask and the collapsed line never carries the
//! length.

use std::io::{IsTerminal, Write, stdout};

use crossterm::cursor::{Hide, MoveToColumn, MoveUp, Show};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, read,
};
use crossterm::style::Stylize;
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode, size};
use crossterm::{execute, queue};

use super::{Cancelled, Choice, Prompter, Tone};

/// A prompter over this process's stdin and its terminal.
pub struct Terminal {
    out: Box<dyn Write + Send>,
    color: bool,
}

/// Where prompts are drawn: stdout when it is the terminal, else the controlling
/// terminal itself. A wrapper that pipes stdout through itself (`op run` masks
/// secrets that way) still leaves the person at a terminal; `None` is a process
/// with no terminal at all.
fn terminal_output() -> Option<Box<dyn Write + Send>> {
    if stdout().is_terminal() {
        return Some(Box::new(stdout()));
    }
    let tty = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .ok()?;
    Some(Box::new(tty))
}

/// What a key press meant to a prompt.
enum Key {
    Up,
    Down,
    Enter,
    Backspace,
    /// Ctrl-U: clear the line.
    Kill,
    Cancel,
    Char(char),
    Paste(String),
    Other,
}

/// Raw mode for the life of a prompt. Dropping restores cooked mode, the
/// cursor, and bracketed paste, so an early return leaves the shell usable.
struct Raw;

impl Raw {
    fn enter(out: &mut impl Write) -> std::io::Result<Self> {
        enable_raw_mode()?;
        execute!(out, EnableBracketedPaste)?;
        Ok(Self)
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        if let Some(mut out) = terminal_output() {
            let _ = execute!(out, DisableBracketedPaste, Show);
        }
        let _ = disable_raw_mode();
    }
}

impl Terminal {
    /// Open the terminal as a prompter. Refuses a stdin that is not a terminal,
    /// or a process with no terminal to draw on: the prompts need raw mode and a
    /// place to redraw.
    pub fn open() -> Result<Self, String> {
        let out = std::io::stdin()
            .is_terminal()
            .then(terminal_output)
            .flatten()
            .ok_or("this prompt is interactive; run efference in a terminal")?;
        Ok(Self {
            out,
            color: std::env::var_os("NO_COLOR").is_none(),
        })
    }

    fn width(&self) -> usize {
        size()
            .map(|(columns, _)| columns as usize)
            .unwrap_or(80)
            .max(20)
    }

    fn paint(&self, text: &str, style: fn(&str) -> String) -> String {
        if self.color {
            style(text)
        } else {
            text.to_string()
        }
    }

    fn question(&self, text: &str) -> String {
        format!(
            "{} {}",
            self.paint("?", |s| s.cyan().bold().to_string()),
            self.paint(text, |s| s.bold().to_string())
        )
    }

    /// The one line an answered prompt leaves behind.
    fn answered(&mut self, question: &str, answer: &str) -> std::io::Result<()> {
        let mark = self.paint("✓", |s| s.green().to_string());
        let answer = self.paint(answer, |s| s.cyan().to_string());
        writeln!(self.out, "{mark} {question} {answer}")
    }

    fn read_key() -> std::io::Result<Key> {
        loop {
            match read()? {
                Event::Key(KeyEvent {
                    code,
                    modifiers,
                    kind: KeyEventKind::Press,
                    ..
                }) => {
                    return Ok(match (code, modifiers) {
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
                            Key::Cancel
                        }
                        (KeyCode::Char('u'), KeyModifiers::CONTROL) => Key::Kill,
                        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => Key::Up,
                        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => Key::Down,
                        (KeyCode::Enter, _) => Key::Enter,
                        (KeyCode::Backspace, _) => Key::Backspace,
                        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                            Key::Char(c)
                        }
                        _ => Key::Other,
                    });
                }
                Event::Paste(text) => return Ok(Key::Paste(text)),
                _ => {}
            }
        }
    }

    /// A key read for a text field: `j`/`k` are letters there, not movement.
    fn read_text_key() -> std::io::Result<Key> {
        Ok(match Self::read_key()? {
            Key::Up => Key::Char('k'),
            Key::Down => Key::Char('j'),
            key => key,
        })
    }

    /// Draw the menu below the question and return how many lines it took. A
    /// trailer that does not fit beside its label continues on indented lines,
    /// so nothing is cut off at the terminal's edge.
    fn draw_menu(&mut self, choices: &[Choice], selected: usize) -> std::io::Result<u16> {
        let width = self.width();
        let label_width = choices
            .iter()
            .map(|choice| choice.label.chars().count())
            .max()
            .unwrap_or(0);
        let mut lines = 0u16;
        for (index, choice) in choices.iter().enumerate() {
            let cursor = if index == selected { "❯" } else { " " };
            let trailer = match (&choice.unavailable, &choice.detail) {
                (Some(why), _) => format!("({why})"),
                (None, Some(detail)) => detail.clone(),
                (None, None) => String::new(),
            };
            let head = format!("{cursor} {:label_width$}  ", choice.label);
            let indent = head.chars().count();
            let trailer_width = width.saturating_sub(indent + 1).max(16);
            let mut rows = wrap(&trailer, trailer_width).into_iter();
            let first = format!("{head}{}", rows.next().unwrap_or_default());
            let mut all = vec![first];
            all.extend(rows.map(|row| format!("{}{row}", " ".repeat(indent))));
            for line in all {
                let line: String = line.chars().take(width.saturating_sub(1)).collect();
                let painted = if choice.unavailable.is_some() {
                    self.paint(&line, |s| s.dim().to_string())
                } else if index == selected {
                    self.paint(&line, |s| s.cyan().to_string())
                } else {
                    line
                };
                queue!(self.out, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                write!(self.out, "{painted}\r\n")?;
                lines += 1;
            }
        }
        let hint = self.paint("  ↑↓ move · enter picks · esc cancels", |s| {
            s.dim().to_string()
        });
        queue!(self.out, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        write!(self.out, "{hint}\r\n")?;
        lines += 1;
        self.out.flush()?;
        Ok(lines)
    }

    /// Read a line in raw mode, echoing `mask` per character when given. The
    /// prompt line is already on screen; this returns the text and leaves the
    /// cursor at the line's end.
    fn read_line(&mut self, mask: Option<char>) -> Result<Option<String>, std::io::Error> {
        let mut text = String::new();
        let mut shown = 0usize;
        loop {
            let key = Self::read_text_key()?;
            match key {
                Key::Enter => return Ok(Some(text)),
                Key::Cancel => return Ok(None),
                Key::Backspace => {
                    if text.pop().is_some() {
                        write!(self.out, "\u{8} \u{8}")?;
                        shown = shown.saturating_sub(1);
                    }
                }
                Key::Kill => {
                    text.clear();
                    let erase = "\u{8} \u{8}".repeat(shown);
                    write!(self.out, "{erase}")?;
                    shown = 0;
                }
                Key::Char(c) => {
                    text.push(c);
                    write!(self.out, "{}", mask.unwrap_or(c))?;
                    shown += 1;
                }
                Key::Paste(pasted) => {
                    let pasted: String = pasted.chars().filter(|c| !c.is_control()).collect();
                    let count = pasted.chars().count();
                    match mask {
                        Some(mask) => write!(self.out, "{}", mask.to_string().repeat(count))?,
                        None => write!(self.out, "{pasted}")?,
                    }
                    shown += count;
                    text.push_str(&pasted);
                }
                Key::Up | Key::Down | Key::Other => {}
            }
            self.out.flush()?;
        }
    }
}

/// An I/O failure mid-prompt is treated as the person leaving: there is no
/// terminal left to ask again on.
fn io_cancelled(_: std::io::Error) -> Cancelled {
    Cancelled
}

impl Prompter for Terminal {
    fn say(&mut self, tone: Tone, text: &str) {
        let width = self.width().min(88);
        let (mark, style): (&str, fn(&str) -> String) = match tone {
            Tone::Plain => (" ", |s| s.to_string()),
            Tone::Good => ("✓", |s| s.green().to_string()),
            Tone::Note => ("·", |s| s.yellow().to_string()),
            Tone::Bad => ("✗", |s| s.red().to_string()),
        };
        let mut first = true;
        for paragraph in text.split('\n') {
            // A numbered step or an indented line keeps its indent on continuation
            // lines, so a wrapped step reads as one item.
            let hang = hanging_indent(paragraph);
            let body = paragraph.trim_start();
            let lead = " ".repeat(paragraph.len() - body.len());
            for (row, line) in wrap(body, width.saturating_sub(2 + hang))
                .into_iter()
                .enumerate()
            {
                let mark = if first {
                    self.paint(mark, style)
                } else {
                    " ".to_string()
                };
                first = false;
                let indent = if row == 0 {
                    lead.clone()
                } else {
                    " ".repeat(hang)
                };
                let _ = writeln!(self.out, "{mark} {indent}{line}");
            }
        }
        let _ = self.out.flush();
    }

    fn select(&mut self, question: &str, choices: &[Choice]) -> Result<usize, Cancelled> {
        let selectable: Vec<usize> = (0..choices.len())
            .filter(|index| choices[*index].unavailable.is_none())
            .collect();
        let Some(&first) = selectable.first() else {
            return Err(Cancelled);
        };
        let mut selected = first;
        let question_line = self.question(question);
        writeln!(self.out, "{question_line}").map_err(io_cancelled)?;
        let _raw = Raw::enter(&mut self.out).map_err(io_cancelled)?;
        execute!(self.out, Hide).map_err(io_cancelled)?;
        let mut lines = self.draw_menu(choices, selected).map_err(io_cancelled)?;
        let outcome = loop {
            match Self::read_key().map_err(io_cancelled)? {
                Key::Up => {
                    let at = selectable.iter().position(|i| *i == selected).unwrap_or(0);
                    selected = selectable[(at + selectable.len() - 1) % selectable.len()];
                }
                Key::Down => {
                    let at = selectable.iter().position(|i| *i == selected).unwrap_or(0);
                    selected = selectable[(at + 1) % selectable.len()];
                }
                Key::Char(digit) if digit.is_ascii_digit() => {
                    let index = (digit as u8 - b'1') as usize;
                    if selectable.contains(&index) {
                        selected = index;
                    }
                }
                Key::Enter => break Ok(selected),
                Key::Cancel => break Err(Cancelled),
                _ => continue,
            }
            queue!(self.out, MoveUp(lines)).map_err(io_cancelled)?;
            lines = self.draw_menu(choices, selected).map_err(io_cancelled)?;
        };
        queue!(
            self.out,
            MoveUp(lines + 1),
            MoveToColumn(0),
            Clear(ClearType::FromCursorDown)
        )
        .map_err(io_cancelled)?;
        drop(_raw);
        match outcome {
            Ok(index) => {
                self.answered(question, &choices[index].label)
                    .map_err(io_cancelled)?;
                Ok(index)
            }
            Err(cancelled) => {
                writeln!(self.out, "{question_line}").map_err(io_cancelled)?;
                Err(cancelled)
            }
        }
    }

    fn text(&mut self, question: &str, default: Option<&str>) -> Result<String, Cancelled> {
        let question_line = self.question(question);
        let hint = match default {
            Some(default) => self.paint(&format!(" ({default})"), |s| s.dim().to_string()),
            None => String::new(),
        };
        write!(self.out, "{question_line}{hint} ").map_err(io_cancelled)?;
        self.out.flush().map_err(io_cancelled)?;
        let raw = Raw::enter(&mut self.out).map_err(io_cancelled)?;
        let typed = self.read_line(None).map_err(io_cancelled)?;
        drop(raw);
        queue!(self.out, MoveToColumn(0), Clear(ClearType::CurrentLine)).map_err(io_cancelled)?;
        let Some(typed) = typed else {
            writeln!(self.out, "{question_line}").map_err(io_cancelled)?;
            return Err(Cancelled);
        };
        let answer = if typed.trim().is_empty() {
            default.unwrap_or("").to_string()
        } else {
            typed
        };
        self.answered(question, &answer).map_err(io_cancelled)?;
        Ok(answer)
    }

    fn secret(&mut self, question: &str) -> Result<String, Cancelled> {
        let question_line = self.question(question);
        write!(self.out, "{question_line} ").map_err(io_cancelled)?;
        self.out.flush().map_err(io_cancelled)?;
        let raw = Raw::enter(&mut self.out).map_err(io_cancelled)?;
        let typed = self.read_line(Some('•')).map_err(io_cancelled)?;
        drop(raw);
        queue!(self.out, MoveToColumn(0), Clear(ClearType::CurrentLine)).map_err(io_cancelled)?;
        let Some(typed) = typed else {
            writeln!(self.out, "{question_line}").map_err(io_cancelled)?;
            return Err(Cancelled);
        };
        self.answered(question, "••••••••").map_err(io_cancelled)?;
        Ok(typed)
    }

    fn confirm(&mut self, question: &str, default: bool) -> Result<bool, Cancelled> {
        let question_line = self.question(question);
        let hint = self.paint(if default { " (Y/n)" } else { " (y/N)" }, |s| {
            s.dim().to_string()
        });
        write!(self.out, "{question_line}{hint} ").map_err(io_cancelled)?;
        self.out.flush().map_err(io_cancelled)?;
        let raw = Raw::enter(&mut self.out).map_err(io_cancelled)?;
        let answer = loop {
            match Self::read_text_key().map_err(io_cancelled)? {
                Key::Char('y' | 'Y') => break Some(true),
                Key::Char('n' | 'N') => break Some(false),
                Key::Enter => break Some(default),
                Key::Cancel => break None,
                _ => {}
            }
        };
        drop(raw);
        queue!(self.out, MoveToColumn(0), Clear(ClearType::CurrentLine)).map_err(io_cancelled)?;
        let Some(answer) = answer else {
            writeln!(self.out, "{question_line}").map_err(io_cancelled)?;
            return Err(Cancelled);
        };
        self.answered(question, if answer { "yes" } else { "no" })
            .map_err(io_cancelled)?;
        Ok(answer)
    }
}

/// How far a paragraph's continuation lines indent: its own leading spaces,
/// plus a leading `1. ` list marker's width.
fn hanging_indent(paragraph: &str) -> usize {
    let lead = paragraph.len() - paragraph.trim_start().len();
    let body = paragraph.trim_start();
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    let marker = if digits > 0 && body[digits..].starts_with(". ") {
        digits + 2
    } else {
        0
    };
    lead + marker
}

/// Greedy word wrap at `width` columns; a word longer than the width stands
/// alone on its line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}
