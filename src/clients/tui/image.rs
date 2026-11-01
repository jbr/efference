//! Result-image state for the TUI (docs/tui.md): decoded pixels and their
//! per-terminal encodings, held beside the transcript rather than in it.
//!
//! A [`DocumentNode::Image`](crate::clients::tui::document::DocumentNode::Image) carries only
//! the log address and pixel dimensions; everything stateful — the fetch lifecycle,
//! the decoded [`DynamicImage`], the protocol encodings — lives here, keyed by that
//! address. Beyond keeping the projection pure of the terminal's graphics capability,
//! the split is what keeps `DocumentNode`'s `Debug`/`Clone` derives: a [`Protocol`]
//! implements neither `Debug` nor `PartialEq`.

use std::collections::HashMap;
use std::fmt;

use image::DynamicImage;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::Widget;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};

/// The log address of one result image: the `ToolResult` entry's `seq` and the
/// image's position in its `images` array — the same coordinates
/// `GET /log/:seq/images/:idx` dereferences (docs/control-api.md).
pub(crate) type ImageKey = (u64, usize);

/// Cap on the rows an image may claim, so a tall image is a figure in the
/// transcript rather than a takeover of it. Scaling is sizing, not truncation —
/// nothing is cut, and the raw bytes stay a fetch away.
const MAX_IMAGE_ROWS: u16 = 24;

/// Every result image the transcript references, in whatever state its bytes
/// have reached. The picker is the terminal's graphics capability, fixed at
/// startup (docs/tui.md — detection is a startup act): `None` means images are
/// off, and image nodes render as a one-line reference instead of pixels.
#[derive(Debug, Default)]
pub(crate) struct ImageStore {
    picker: Option<Picker>,
    slots: HashMap<ImageKey, Slot>,
}

/// One image's lifecycle. There is no "encoded" state apart from `Ready` —
/// encodings are derived lazily (the footprint depends on the render width,
/// which only the render walk knows) and cached inside the slot.
enum Slot {
    /// Bytes requested over the control channel; not yet arrived.
    Fetching,
    /// The fetch, decode, or encode failed; the reserved rows say so.
    Failed(String),
    /// Decoded pixels, plus the encodings at the footprint last rendered.
    Ready {
        source: DynamicImage,
        encoded: Option<Encoded>,
    },
}

/// The per-protocol encodings of one image at one cell footprint.
struct Encoded {
    /// The footprint the encodings were made for; a resize misses and re-encodes.
    cells: Size,
    /// The detected protocol's encoding — what paints when the image is fully
    /// visible. `None` when the capability *is* halfblocks: the scratch buffer
    /// then serves every visibility state.
    native: Option<Protocol>,
    /// The same pixels as halfblock character cells, at the same footprint.
    /// Ordinary cells, so the renderer can blit any row range of them — the
    /// partial-scroll path (docs/tui.md).
    scratch: Buffer,
}

/// What the renderer should paint for an image, borrowed from the store.
pub(crate) enum Painted<'s> {
    /// Bytes not here yet (or the encode hasn't run) — a placeholder line.
    Pending,
    /// The image can't be shown; the reserved rows carry the reason.
    Failed(&'s str),
    Ready {
        native: Option<&'s Protocol>,
        scratch: &'s Buffer,
    },
}

impl ImageStore {
    /// Install the terminal's graphics capability (docs/tui.md — a startup act,
    /// then a plain parameter of rendering). `None` turns images off.
    pub(crate) fn set_picker(&mut self, picker: Option<Picker>) {
        self.picker = picker;
    }

    /// Match the encoder's padding color to the theme background. Aspect-fit
    /// leaves sub-cell padding around the scaled image, and the crate's default
    /// pad (transparent black) renders as an opaque gray band under iTerm2,
    /// which composites transparency wrong. Only truecolor themes carry an RGB
    /// to match; anything else keeps the default. Cached encodings are dropped
    /// so a theme change recolors the padding on the next paint.
    pub(crate) fn set_theme_background(&mut self, color: Option<ratatui::style::Color>) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        let ratatui::style::Color::Rgb(r, g, b) = color.unwrap_or(ratatui::style::Color::Reset)
        else {
            return;
        };
        picker.set_background_color(Some(image::Rgba([r, g, b, 255])));
        for slot in self.slots.values_mut() {
            if let Slot::Ready { encoded, .. } = slot {
                *encoded = None;
            }
        }
    }

    /// The cell footprint an image reserves at up to `max_cols` columns: its
    /// natural size in cells (pixels over the terminal's font size), scaled down
    /// to fit `max_cols` × [`MAX_IMAGE_ROWS`] preserving aspect. A pure function
    /// of the metadata and the width — never of whether bytes have arrived — so
    /// the reservation cannot reflow when they do. `None` when images are off.
    pub(crate) fn footprint(&self, px_width: u32, px_height: u32, max_cols: u16) -> Option<Size> {
        self.footprint_within(px_width, px_height, max_cols, MAX_IMAGE_ROWS)
    }

    /// [`footprint`](Self::footprint) under a row cap of the caller's — a staged
    /// attachment's thumbnail is a few rows of chrome, not a transcript figure.
    pub(crate) fn footprint_within(
        &self,
        px_width: u32,
        px_height: u32,
        max_cols: u16,
        max_rows: u16,
    ) -> Option<Size> {
        let font = self.picker.as_ref()?.font_size();
        // Floor, not ceil: a ceil'd final row is mostly padding (at an 18×36
        // font, 400 px is 11.1 rows — the 12th would be 90% background), and
        // padding renders as a visible band under protocols that composite the
        // pad color opaquely (iTerm2). Rounding down scales the image a hair
        // smaller instead of reserving a nearly-empty row.
        let cols = (px_width / u32::from(font.width.max(1))).max(1);
        let rows = (px_height / u32::from(font.height.max(1))).max(1);
        let max_cols = u32::from(max_cols.max(1));
        let max_rows = u32::from(max_rows.max(1));
        let scale = 1.0_f64
            .min(max_cols as f64 / cols as f64)
            .min(max_rows as f64 / rows as f64);
        let w = ((cols as f64 * scale).round() as u32).clamp(1, max_cols);
        let h = ((rows as f64 * scale).round() as u32).clamp(1, max_rows);
        Some(Size::new(w as u16, h as u16))
    }

    /// Whether `key` has no slot yet — the caller should request its bytes and
    /// [`note_fetching`](Self::note_fetching). Also gates the inline-decode path,
    /// so a re-delivered entry (a reattach replay) doesn't decode twice.
    pub(crate) fn needs_fetch(&self, key: ImageKey) -> bool {
        !self.slots.contains_key(&key)
    }

    /// Whether images render at all — a graphics capability was detected.
    pub(crate) fn enabled(&self) -> bool {
        self.picker.is_some()
    }

    /// The decoded pixel size of `key`, once its bytes have landed.
    pub(crate) fn source_size(&self, key: ImageKey) -> Option<(u32, u32)> {
        match self.slots.get(&key) {
            Some(Slot::Ready { source, .. }) => Some((source.width(), source.height())),
            _ => None,
        }
    }

    /// Forget `key` — a staged thumbnail whose chip was removed or sent.
    pub(crate) fn remove(&mut self, key: ImageKey) {
        self.slots.remove(&key);
    }

    /// Record that `key`'s bytes have been requested.
    pub(crate) fn note_fetching(&mut self, key: ImageKey) {
        self.slots.entry(key).or_insert(Slot::Fetching);
    }

    /// Land a fetch's outcome: decoded pixels, or the reason there are none.
    pub(crate) fn insert(&mut self, key: ImageKey, image: Result<DynamicImage, String>) {
        let slot = match image {
            Ok(source) => Slot::Ready {
                source,
                encoded: None,
            },
            Err(why) => Slot::Failed(why),
        };
        self.slots.insert(key, slot);
    }

    /// Decode an image whose base64 bytes rode the entry itself — the offline
    /// path (`efference dev view`, the gallery), where `persist::load` delivers the
    /// full log entry rather than a data-omitted view (docs/control-api.md).
    pub(crate) fn insert_inline(&mut self, key: ImageKey, data: &str) {
        use base64::Engine;
        let result = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|e| e.to_string())
            .and_then(|bytes| image::load_from_memory(&bytes).map_err(|e| e.to_string()));
        self.insert(key, result);
    }

    /// Make sure `key`'s encodings exist at `cells`, encoding (or re-encoding,
    /// after a resize) if they don't. A failed encode moves the slot to
    /// `Failed` — the pixels can't be shown, and retrying wouldn't change that.
    pub(crate) fn ensure_encoded(&mut self, key: ImageKey, cells: Size) {
        let Some(picker) = &self.picker else { return };
        let Some(slot) = self.slots.get_mut(&key) else {
            return;
        };
        let Slot::Ready { source, encoded } = &*slot else {
            return;
        };
        if encoded.as_ref().is_some_and(|e| e.cells == cells) {
            return;
        }
        match encode(picker, source, cells) {
            Ok(enc) => {
                if let Slot::Ready { encoded, .. } = slot {
                    *encoded = Some(enc);
                }
            }
            Err(why) => *slot = Slot::Failed(why),
        }
    }

    /// Whether the capability's native render **flashes when repositioned**:
    /// the protocol has no stored-image/placement concept, so every move
    /// retransmits and re-decodes the whole payload (iTerm2's OSC 1337, sixel).
    /// Kitty stores the image and re-places it, so motion is smooth there. The
    /// renderer holds the halfblocks slice during scroll for the flashing
    /// protocols (docs/tui.md).
    pub(crate) fn native_flickers_on_move(&self) -> bool {
        self.picker.as_ref().is_some_and(|p| {
            matches!(
                p.protocol_type(),
                ProtocolType::Iterm2 | ProtocolType::Sixel
            )
        })
    }

    /// What the renderer should paint for `key`, after
    /// [`ensure_encoded`](Self::ensure_encoded).
    pub(crate) fn painted(&self, key: ImageKey) -> Painted<'_> {
        match self.slots.get(&key) {
            None | Some(Slot::Fetching) | Some(Slot::Ready { encoded: None, .. }) => {
                Painted::Pending
            }
            Some(Slot::Failed(why)) => Painted::Failed(why),
            Some(Slot::Ready {
                encoded: Some(enc), ..
            }) => Painted::Ready {
                native: enc.native.as_ref(),
                scratch: &enc.scratch,
            },
        }
    }
}

/// Encode one image at one footprint: the native protocol (unless the
/// capability is halfblocks, where the scratch covers everything), and the
/// halfblocks scratch buffer rendered once through the real widget so its
/// cells are exactly what a halfblocks terminal would show.
fn encode(picker: &Picker, source: &DynamicImage, cells: Size) -> Result<Encoded, String> {
    let native = match picker.protocol_type() {
        ProtocolType::Halfblocks => None,
        _ => Some(
            picker
                .new_protocol(source.clone(), cells, Resize::Fit(None))
                .map_err(|e| e.to_string())?,
        ),
    };
    let mut halfblocks = picker.clone();
    halfblocks.set_protocol_type(ProtocolType::Halfblocks);
    let protocol = halfblocks
        .new_protocol(source.clone(), cells, Resize::Fit(None))
        .map_err(|e| e.to_string())?;
    let size = protocol.size();
    let area = Rect::new(0, 0, size.width, size.height);
    let mut scratch = Buffer::empty(area);
    Image::new(&protocol).render(area, &mut scratch);
    Ok(Encoded {
        cells,
        native,
        scratch,
    })
}

// Hand-written: `Protocol` (inside `Encoded`) implements no `Debug`, and
// `InteractiveState` derives it.
impl fmt::Debug for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Slot::Fetching => write!(f, "Fetching"),
            Slot::Failed(why) => f.debug_tuple("Failed").field(why).finish(),
            Slot::Ready { encoded, .. } => f
                .debug_struct("Ready")
                .field("encoded", &encoded.is_some())
                .finish(),
        }
    }
}

#[cfg(test)]
mod test;
