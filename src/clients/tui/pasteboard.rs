//! Whether the clipboard holds an image, learned from metadata alone (docs/tui.md).
//!
//! A terminal's own paste carries text only, so a copied image reaches the TUI as nothing and
//! the operator has to be told `C-v` will fetch it. Telling them must not mean *reading* the
//! clipboard: whatever was copied last — a screenshot of something private, say — is not the
//! TUI's to look at until the operator asks. macOS exposes the pasteboard's change count and the
//! list of types its contents offer without reading them (and without the pasteboard privacy
//! alert); that is all this uses. Elsewhere there is no such view through arboard, so there is
//! no hint and `C-v` stands alone.

/// Whether a pasteboard offering `types` holds an image the operator would paste as one: an
/// image type, and neither plain text (text is what a paste means when there is any) nor a file
/// URL (a file copied in Finder offers its icon as an image).
pub(crate) fn holds_image<'t>(types: impl IntoIterator<Item = &'t str>) -> bool {
    let mut image = false;
    for ty in types {
        match ty {
            "public.utf8-plain-text" | "public.file-url" => return false,
            "public.png"
            | "public.tiff"
            | "public.jpeg"
            | "org.webmproject.webp"
            | "com.compuserve.gif"
            | "public.heic" => image = true,
            _ => {}
        }
    }
    image
}

/// Polls the pasteboard's change count; reports its image-ness when it changes.
#[derive(Debug, Default)]
pub(crate) struct Watch {
    last: Option<isize>,
}

impl Watch {
    /// Whether this platform can watch at all — a poll loop elsewhere would only spin.
    pub(crate) const SUPPORTED: bool = cfg!(target_os = "macos");

    /// `Some(holds_image)` on the first poll and whenever the clipboard has changed since the
    /// last; `None` when nothing changed.
    #[cfg(target_os = "macos")]
    pub(crate) fn poll(&mut self) -> Option<bool> {
        use objc2_app_kit::NSPasteboard;
        objc2::rc::autoreleasepool(|_| {
            let pasteboard = NSPasteboard::generalPasteboard();
            let count = pasteboard.changeCount();
            if self.last == Some(count) {
                return None;
            }
            self.last = Some(count);
            let types: Vec<String> = pasteboard
                .types()
                .map_or_default(|types| types.iter().map(|ty| ty.to_string()).collect());
            Some(holds_image(types.iter().map(String::as_str)))
        })
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn poll(&mut self) -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// The type lists real copies offer (2026-09-27): a browser's "copy image", a screenshot,
    /// a text copy that also carries a rendering, and a Finder file copy.
    #[test]
    fn an_image_is_an_image_type_with_no_text_or_file() {
        // Chrome's "copy image", as the pasteboard listed it.
        let browser_image = [
            "public.png",
            "Apple PNG pasteboard type",
            "public.html",
            "Apple HTML pasteboard type",
            "org.chromium.internal.source-rfh-token",
            "org.chromium.source-url",
            "public.tiff",
            "NeXT TIFF v4.0 pasteboard type",
        ];
        assert!(holds_image(browser_image));
        assert!(holds_image(["public.png"]));
        assert!(!holds_image([
            "public.rtf",
            "public.utf8-plain-text",
            "public.tiff"
        ]));
        assert!(!holds_image([
            "public.file-url",
            "public.tiff",
            "public.utf8-plain-text"
        ]));
        assert!(!holds_image(["public.file-url", "public.tiff"]));
        assert!(!holds_image([]));
    }
}
