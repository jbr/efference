//! Clipboard access for the interactive TUI.
//!
//! The composer remains deterministic and has no operating-system side effects.
//! This adapter owns one persistent system clipboard plus an in-process fallback,
//! which keeps copy/cut usable when the platform clipboard cannot be reached.
//!
//! Images are read as well as text: a pasted screenshot is an attachment
//! (docs/inbox.md), so it is re-encoded as a PNG and staged for the next turn rather than
//! pushed into the conversation.

use arboard::{Clipboard, Error};

/// Result of trying to read from the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Paste {
    /// UTF-8 text is available.
    Text(String),
    /// A bitmap is available, already encoded as PNG bytes — the store's format, chosen here
    /// because the clipboard hands over raw RGBA and something has to name an encoding.
    Image(Vec<u8>),
    /// The clipboard holds an image that could not be read or re-encoded; the reason, for the
    /// status line — a silent empty paste is what this replaced.
    ImageUnreadable(String),
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
        // Text first: a copy from an editor often carries a bitmap flavour too, and text is
        // what the operator meant. Blank text is not text — a browser's "copy image" can carry
        // an empty text flavour beside the picture — so only non-blank text wins.
        let text = system.get_text();
        if let Ok(text) = &text
            && !text.trim().is_empty()
        {
            self.fallback = Some(text.clone());
            return Paste::Text(text.clone());
        }
        match system.get_image() {
            Ok(image) => {
                return match encode_png(&image) {
                    Some(png) => Paste::Image(png),
                    None => Paste::ImageUnreadable(format!(
                        "a {}×{} image whose bytes do not match its size",
                        image.width, image.height
                    )),
                };
            }
            Err(Error::ContentNotAvailable) => {}
            Err(e) => return Paste::ImageUnreadable(e.to_string()),
        }
        match text {
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

/// Re-encode the clipboard's raw RGBA as PNG. `None` when the buffer does not describe an
/// image of the stated size — a clipboard that lies is a skipped paste, not a panic.
fn encode_png(image: &arboard::ImageData<'_>) -> Option<Vec<u8>> {
    let buffer = ::image::RgbaImage::from_raw(
        u32::try_from(image.width).ok()?,
        u32::try_from(image.height).ok()?,
        image.bytes.to_vec(),
    )?;
    let mut png = Vec::new();
    ::image::DynamicImage::ImageRgba8(buffer)
        .write_to(
            &mut std::io::Cursor::new(&mut png),
            ::image::ImageFormat::Png,
        )
        .ok()?;
    Some(png)
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
