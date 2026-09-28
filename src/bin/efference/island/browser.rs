//! The island in the browser (docs/operating.md): the same terminal-owned host,
//! with a loopback web bind, and the web client opened on the new session.
//!
//! The owner is this foreground process, never the browser tab: a tab's reloads
//! and stream reconnects must not end the island, so the ownership pipe stays
//! here and the terminal holds a one-line prompt until the person quits. Every
//! link is minted when asked for, because a bootstrap link's code is single-use
//! and short-lived (docs/web-bind.md); the cookie it trades for is what keeps a
//! tab signed in afterwards.

use super::*;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, poll, read};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::io::Write;

/// Loopback at a port the kernel picks; the host reports the one it bound, and
/// its minted links name it.
const BIND: &str = "127.0.0.1:0";

/// How often the prompt checks that the host is still running between keys.
const KEY_POLL: Duration = Duration::from_millis(200);

/// Start the island with a web bind, create a session here, open the browser on
/// it, and hold the prompt until the person quits or the host ends.
pub(crate) async fn run(creation: Creation) -> CliResult {
    let Some((mut island, id)) = start(Some(BIND), creation).await? else {
        return Ok(());
    };
    let links = Links {
        client: ControlClient::new(island.socket()),
        session: id,
    };
    let held = hold(&mut island, &links).await;
    island.end().await;
    held
}

/// Mints links to the created session.
struct Links {
    client: ControlClient,
    session: String,
}

impl Links {
    /// A fresh single-use link to the session's page.
    async fn mint(&self) -> Result<String, CliError> {
        let link = self.client.auth_link().await?;
        Ok(session_link(&link, &self.session))
    }
}

/// `http://localhost:1234/#token=…` → `http://localhost:1234/sessions/<id>/#token=…`:
/// the web client reads the token on any path, so the link lands on the session
/// rather than on the list.
fn session_link(link: &str, session: &str) -> String {
    match link.split_once("/#") {
        Some((base, fragment)) => format!("{base}/sessions/{session}/#{fragment}"),
        None => link.to_string(),
    }
}

/// What a key asks for at the prompt.
enum Ask {
    Open,
    Copy,
    Print,
    Quit,
}

fn ask(key: KeyEvent) -> Option<Ask> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('o') | KeyCode::Enter, _) => Some(Ask::Open),
        (KeyCode::Char('c'), KeyModifiers::NONE) => Some(Ask::Copy),
        (KeyCode::Char('p'), _) => Some(Ask::Print),
        (KeyCode::Char('q') | KeyCode::Esc, _) => Some(Ask::Quit),
        (KeyCode::Char('c' | 'd'), KeyModifiers::CONTROL) => Some(Ask::Quit),
        _ => None,
    }
}

/// Open the browser once, then answer keys until the person quits or the host
/// ends. Everything drawn is erased on the way out, and raw mode is restored on
/// every path, so the terminal is left as it was found (docs/operating.md).
async fn hold(island: &mut Island, links: &Links) -> CliResult {
    let first = links.mint().await?;
    let opened = webbrowser::open(&first).is_ok();
    let mut screen = Screen::default();
    enable_raw_mode()?;
    if !opened {
        screen.line(&format!("could not open a browser; the link: {first}"));
    }
    screen.line(PROMPT);
    let outcome = answer(island, links, &mut screen).await;
    screen.erase();
    let _ = disable_raw_mode();
    outcome
}

/// The prompt's key loop, returning on quit, on the host ending, or on a
/// terminal error — the caller restores the terminal either way.
async fn answer(island: &mut Island, links: &Links, screen: &mut Screen) -> CliResult {
    loop {
        if island.child.try_wait()?.is_some() {
            return Err("the host ended".into());
        }
        if !poll(KEY_POLL)? {
            continue;
        }
        let Event::Key(key) = read()? else { continue };
        let Some(asked) = ask(key) else { continue };
        let note = match asked {
            Ask::Quit => return Ok(()),
            Ask::Open => match links.mint().await {
                Ok(link) if webbrowser::open(&link).is_ok() => {
                    "opened a new link in your browser".to_string()
                }
                Ok(link) => format!("could not open a browser; the link: {link}"),
                Err(error) => format!("could not mint a link: {error}"),
            },
            Ask::Copy => match links.mint().await {
                Ok(link) => match copy(&link) {
                    Ok(()) => {
                        "copied a new link — it works once, within a minute or so".to_string()
                    }
                    Err(error) => format!("could not copy ({error}); the link: {link}"),
                },
                Err(error) => format!("could not mint a link: {error}"),
            },
            Ask::Print => match links.mint().await {
                Ok(link) => format!("{link}  (works once, within a minute or so)"),
                Err(error) => format!("could not mint a link: {error}"),
            },
        };
        screen.replace_last(&note);
        screen.line(PROMPT);
    }
}

const PROMPT: &str =
    "efference is open in your browser · o open again · c copy link · p print link · q quit";

fn copy(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text.to_string()))
        .map_err(|error| error.to_string())
}

/// The lines the prompt has drawn, counted in screen rows so they can be erased.
/// Raw mode is on, so every line ends in `\r\n`.
#[derive(Default)]
struct Screen {
    /// Rows per drawn line, in order.
    rows: Vec<usize>,
}

impl Screen {
    fn line(&mut self, text: &str) {
        let mut err = std::io::stderr();
        let _ = write!(err, "{text}\r\n");
        let _ = err.flush();
        let columns = crossterm::terminal::size()
            .map(|(c, _)| c as usize)
            .unwrap_or(80)
            .max(1);
        let width = unicode_width::UnicodeWidthStr::width(text);
        self.rows.push(width.div_ceil(columns).max(1));
    }

    /// Replace the last line (the prompt) with `text`, which stays above the
    /// prompt drawn next.
    fn replace_last(&mut self, text: &str) {
        if let Some(rows) = self.rows.pop() {
            clear_up(rows);
        }
        self.line(text);
    }

    fn erase(&mut self) {
        clear_up(self.rows.drain(..).sum());
    }
}

/// Move up `rows` rows and clear from there down.
fn clear_up(rows: usize) {
    use crossterm::cursor::{MoveToColumn, MoveUp};
    use crossterm::terminal::{Clear, ClearType};
    if rows == 0 {
        return;
    }
    let up = u16::try_from(rows).unwrap_or(u16::MAX);
    let _ = crossterm::execute!(
        std::io::stderr(),
        MoveUp(up),
        MoveToColumn(0),
        Clear(ClearType::FromCursorDown)
    );
}

#[cfg(test)]
mod test;
