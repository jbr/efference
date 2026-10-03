//! Paste routing and the staged-attachment row (docs/tui.md, docs/inbox.md).
//!
//! What a paste becomes is decided here, once, for every way one arrives — a bracketed paste
//! from the terminal (⌘V, a drag onto the window) or a clipboard read (`Super-V`, `C-y`):
//!
//! - a clipboard image, or an empty paste while the clipboard holds one (terminals pass text
//!   only, so an image-only clipboard pastes as nothing) — staged as a PNG;
//! - exactly one or more existing files, as a terminal types a drag — each staged;
//! - text past [`INLINE_LINE_LIMIT`] lines — staged as a text file;
//! - anything else — typed into the composer.
//!
//! A staged file uploads at once, so its chip shows what the daemon made of it; the next turn
//! carries every ready chip.
//!
//! [`INLINE_LINE_LIMIT`]: crate::attachments::INLINE_LINE_LIMIT

use super::*;
use crate::tui::clipboard::Paste;
use crate::tui::staged::{AttachCmd, dropped_paths};

/// The most one staged file may carry — the daemon's own upload bound.
const STAGE_MAX_BYTES: u64 = 64 * 1024 * 1024;

impl<'a> InteractiveState<'a> {
    /// A bracketed paste from the terminal.
    pub(in crate::tui) fn handle_paste(&mut self, text: String) {
        if let super::UiMode::Sessions(menu) = &mut self.ui_mode
            && let Some(page) = &mut menu.resume
        {
            page.paste(&text);
            return;
        }
        // The new-session form open over the switcher owns a paste as it owns a
        // key: its query and editors take it, and the composer underneath never does.
        if let super::UiMode::Sessions(menu) = &mut self.ui_mode
            && let Some(form) = &mut menu.form
        {
            let action = form.paste(&text);
            self.apply_form_action(action);
            return;
        }
        if text.trim().is_empty() {
            // An image-only clipboard reaches a terminal app as an empty paste, if at all.
            match self.clipboard.paste() {
                Paste::Image(png) => self.stage_clipboard_image(png),
                Paste::ImageUnreadable(why) => {
                    self.set_debug_message(format!("could not read the clipboard image: {why}"))
                }
                _ => self.set_debug_message("nothing to paste"),
            }
            return;
        }
        self.route_text_paste(&text);
    }

    /// Paste from the system clipboard (`Super-V`, `C-y`), falling back to the in-process copy
    /// only when system access fails. Arboard does not distinguish empty from non-text
    /// content, so `ContentNotAvailable` is reported without using stale fallback text.
    pub(in crate::tui) fn paste_clipboard(&mut self) {
        match self.clipboard.paste() {
            Paste::Text(text) if text.trim().is_empty() => {
                self.set_debug_message("the clipboard has no text or image")
            }
            Paste::Text(text) => self.route_text_paste(&text),
            Paste::Image(png) => self.stage_clipboard_image(png),
            Paste::ImageUnreadable(why) => {
                self.set_debug_message(format!("could not read the clipboard image: {why}"))
            }
            Paste::Unavailable => {
                if let Some(text) = self.clipboard.fallback().map(str::to_owned) {
                    self.route_text_paste(&text);
                    self.set_debug_message("pasted (in-process clipboard)");
                } else {
                    self.set_debug_message("clipboard unavailable");
                }
            }
            Paste::NoText => self.set_debug_message("clipboard has no text"),
        }
    }

    fn route_text_paste(&mut self, text: &str) {
        if let Some(paths) = dropped_paths(text) {
            let count = paths.len();
            for path in paths {
                self.stage_path(&path);
            }
            self.set_debug_message(match count {
                1 => "attached a file".to_string(),
                n => format!("attached {n} files"),
            });
            return;
        }
        let lines = text.lines().count();
        if lines <= crate::attachments::INLINE_LINE_LIMIT || self.read_only() {
            self.composer.paste_text(text);
            self.set_debug_message("pasted");
            return;
        }
        let stamp = paste_stamp();
        self.stage(format!("paste-{stamp}.txt"), text.as_bytes().to_vec());
        self.set_debug_message(format!("{lines} lines attached"));
    }

    fn stage_clipboard_image(&mut self, png: Vec<u8>) {
        // Taken: the offer has been used, and the image is on the row now.
        self.clipboard_image = false;
        if self.read_only() {
            self.set_debug_message("a read-only view has nowhere to attach an image");
            return;
        }
        let stamp = paste_stamp();
        self.stage(format!("paste-{stamp}.png"), png);
        self.set_debug_message("image attached");
    }

    fn stage_path(&mut self, path: &std::path::Path) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let too_big = std::fs::metadata(path).is_ok_and(|m| m.len() > STAGE_MAX_BYTES);
        match std::fs::read(path) {
            Ok(_) if too_big => self.set_debug_message(format!("{name} is too large to attach")),
            Ok(bytes) => self.stage(name, bytes),
            Err(e) => self.set_debug_message(format!("could not read {name}: {e}")),
        }
    }

    fn stage(&mut self, name: String, bytes: Vec<u8>) {
        let cmd = self.staged.stage(name, bytes);
        self.send_attach(cmd);
    }

    /// An upload answered ([`TuiEvent::Uploaded`]).
    pub(in crate::tui) fn upload_landed(
        &mut self,
        key: u64,
        result: Result<crate::attachments::Arrival, String>,
    ) {
        if let Err(e) = &result {
            self.set_debug_message(format!("upload failed: {e}"));
        }
        if let Some(cmd) = self.staged.landed(key, result) {
            self.send_attach(cmd);
        }
        self.dirty = true;
    }

    /// Take a staged chip back (its `×`, or `⌫` on an empty composer).
    pub(in crate::tui) fn remove_staged(&mut self, key: u64) {
        self.staged_images.remove((key, 0));
        if let Some(cmd) = self.staged.remove(key) {
            self.send_attach(cmd);
        }
        self.set_debug_message("attachment removed");
        self.dirty = true;
    }

    /// Empty the staged row after a send: the chips and their thumbnails go, the uploads stay
    /// (the sent turn names them).
    pub(in crate::tui) fn clear_staged(&mut self) {
        for staged in self.staged.items() {
            self.staged_images.remove((staged.key, 0));
        }
        self.staged.clear();
    }

    fn send_attach(&mut self, cmd: AttachCmd) {
        if self.attachments_tx.send(cmd).is_err() {
            self.set_debug_message("could not attach (daemon gone?)");
        }
        self.dirty = true;
    }
}

/// A paste's name stamp: seconds since the epoch, which the store keeps unique.
fn paste_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
