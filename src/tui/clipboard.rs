//! Plain-text clipboard access for the interactive TUI.
//!
//! The composer remains deterministic and has no operating-system side effects.
//! This adapter owns one persistent system clipboard plus an in-process fallback,
//! which keeps copy/cut usable when the platform clipboard cannot be reached.

use arboard::{Clipboard, Error};

/// Result of trying to read plain text from the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Paste {
    /// UTF-8 text is available.
    Text(String),
    /// The system clipboard cannot be read; callers may use the fallback.
    Unavailable,
    /// `arboard` reports unavailable text, which conflates empty and incompatible
    /// formats. Treat it as unavailable rather than mislabeling it unsupported.
    NoText,
}

/// TUI-owned clipboard with a process-local plain-text fallback.
pub(super) struct TextClipboard {
    system: Option<Clipboard>,
    fallback: Option<String>,
}

impl std::fmt::Debug for TextClipboard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TextClipboard")
            .field("system_available", &self.system.is_some())
            .field("fallback", &self.fallback)
            .finish()
    }
}

impl TextClipboard {
    /// Open the system clipboard once for the lifetime of the TUI. Failure merely
    /// leaves the process-local fallback available.
    pub(super) fn new() -> Self {
        Self {
            system: Clipboard::new().ok(),
            fallback: None,
        }
    }

    /// A test-only constructor that never touches the host clipboard.
    #[cfg(test)]
    pub(super) fn unavailable() -> Self {
        Self {
            system: None,
            fallback: None,
        }
    }

    /// Remember text before making a best-effort system write.
    pub(super) fn copy(&mut self, text: String) -> Result<(), Error> {
        self.fallback = Some(text.clone());
        self.system
            .as_mut()
            .ok_or(Error::ClipboardNotSupported)?
            .set_text(text)
    }

    /// Get system text when possible. A read failure permits fallback, while
    /// ContentNotAvailable cannot distinguish empty from a non-text format in
    /// arboard 3.6, so it also remains an ordinary unavailable result.
    pub(super) fn paste(&mut self) -> Paste {
        let Some(system) = self.system.as_mut() else {
            return Paste::Unavailable;
        };
        match system.get_text() {
            Ok(text) => {
                self.fallback = Some(text.clone());
                Paste::Text(text)
            }
            Err(Error::ContentNotAvailable) => Paste::NoText,
            Err(_) => Paste::Unavailable,
        }
    }

    pub(super) fn fallback(&self) -> Option<&str> {
        self.fallback.as_deref()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn unavailable_clipboard_keeps_copy_in_process() {
        let mut clipboard = TextClipboard::unavailable();
        assert!(clipboard.copy("copied".into()).is_err());
        assert_eq!(clipboard.fallback(), Some("copied"));
        assert_eq!(clipboard.paste(), Paste::Unavailable);
    }
}
