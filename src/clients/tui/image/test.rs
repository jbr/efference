use super::*;

/// A store whose capability is deterministic halfblocks — the snapshot-stable
/// constructor (docs/tui.md).
fn halfblocks_store() -> ImageStore {
    let mut store = ImageStore::default();
    store.set_picker(Some(Picker::halfblocks()));
    store
}

/// A tiny valid PNG (a red/blue gradient), as encoded bytes.
fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let img = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x * 8) as u8, 0, (y * 8) as u8, 255])
    }));
    let mut bytes = std::io::Cursor::new(Vec::new());
    img.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

fn png_base64(width: u32, height: u32) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(png_bytes(width, height))
}

#[test]
fn footprint_is_none_with_no_picker() {
    let store = ImageStore::default();
    assert!(store.footprint(100, 100, 80).is_none());
}

#[test]
fn footprint_fits_the_caps_and_keeps_aspect() {
    let store = halfblocks_store();
    // A large square image: capped by rows (24), and the width scales with it —
    // square pixels over a taller-than-wide font cell means cols ≈ 2 × rows for
    // the halfblocks picker's font.
    let cells = store.footprint(2400, 2400, 200).unwrap();
    assert_eq!(cells.height, 24);
    assert!(cells.width <= 200);
    // A wide image at a narrow terminal: capped by cols.
    let cells = store.footprint(2400, 100, 40).unwrap();
    assert_eq!(cells.width, 40);
    assert!(cells.height <= 24);
    // Nothing collapses to zero.
    let cells = store.footprint(1, 1, 40).unwrap();
    assert!(cells.width >= 1 && cells.height >= 1);
}

#[test]
fn footprint_is_metadata_only() {
    // The reservation is identical before and after bytes arrive — the
    // no-reflow invariant (docs/tui.md).
    let mut store = halfblocks_store();
    let before = store.footprint(64, 64, 80).unwrap();
    store.insert_inline((7, 0), &png_base64(64, 64));
    assert_eq!(store.footprint(64, 64, 80).unwrap(), before);
}

#[test]
fn inline_decode_encodes_to_halfblock_cells() {
    let mut store = halfblocks_store();
    let key = (3, 0);
    store.insert_inline(key, &png_base64(32, 32));
    let cells = store.footprint(32, 32, 80).unwrap();
    store.ensure_encoded(key, cells);
    match store.painted(key) {
        Painted::Ready { native, scratch } => {
            // Halfblocks capability: the scratch serves every state; no native
            // protocol rides beside it.
            assert!(native.is_none());
            assert!(scratch.area.width <= cells.width);
            assert!(scratch.area.height <= cells.height);
            assert!(scratch.area.width > 0 && scratch.area.height > 0);
            // The cells are the half-block mosaic.
            let cell = scratch.cell((0, 0)).unwrap();
            assert_eq!(cell.symbol(), "▄");
        }
        _ => panic!("expected Ready"),
    }
}

#[test]
fn bad_bytes_fail_without_poisoning_the_reservation() {
    let mut store = halfblocks_store();
    let key = (4, 0);
    store.insert_inline(key, "bm90IGFuIGltYWdl"); // "not an image"
    store.ensure_encoded(key, Size::new(10, 5));
    assert!(matches!(store.painted(key), Painted::Failed(_)));
    // The footprint still answers — the reserved rows just carry the reason.
    assert!(store.footprint(32, 32, 80).is_some());
}

#[test]
fn unfetched_and_fetching_are_pending() {
    let mut store = halfblocks_store();
    let key = (5, 0);
    assert!(store.needs_fetch(key));
    assert!(matches!(store.painted(key), Painted::Pending));
    store.note_fetching(key);
    assert!(!store.needs_fetch(key), "a noted fetch is not re-requested");
    assert!(matches!(store.painted(key), Painted::Pending));
}

#[test]
fn reencode_follows_a_footprint_change() {
    // Large enough (320×320 px = 32×16 cells natural at the halfblocks picker's
    // assumed 10×20 font) that a 10-column budget genuinely constrains it.
    let mut store = halfblocks_store();
    let key = (6, 0);
    store.insert_inline(key, &png_base64(320, 320));
    let narrow = store.footprint(320, 320, 10).unwrap();
    store.ensure_encoded(key, narrow);
    let narrow_width = match store.painted(key) {
        Painted::Ready { scratch, .. } => scratch.area.width,
        _ => panic!("expected Ready"),
    };
    let wide = store.footprint(320, 320, 60).unwrap();
    assert_ne!(narrow, wide);
    store.ensure_encoded(key, wide);
    let wide_width = match store.painted(key) {
        Painted::Ready { scratch, .. } => scratch.area.width,
        _ => panic!("expected Ready"),
    };
    assert!(
        wide_width > narrow_width,
        "a resize re-encodes to the new fit"
    );
}
