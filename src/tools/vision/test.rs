use super::*;
use image::{ImageBuffer, Rgb, Rgba};

/// The worked examples from the vision docs' sizing tables — both tiers,
/// including the case that fits both edges but not the token budget.
#[test]
fn resized_size_matches_the_documented_table() {
    // Standard tier (1568 / 1568).
    assert_eq!(resized_size(200, 200, 1568, 1568), (200, 200));
    assert_eq!(resized_size(1000, 1000, 1568, 1568), (1000, 1000));
    assert_eq!(resized_size(1092, 1092, 1568, 1568), (1092, 1092));
    assert_eq!(resized_size(1920, 1080, 1568, 1568), (1456, 819));
    assert_eq!(resized_size(2000, 1500, 1568, 1568), (1269, 952));
    assert_eq!(resized_size(3840, 2160, 1568, 1568), (1456, 819));
    // The A4-scan case: fits both edges, exceeds the token budget.
    assert_eq!(resized_size(1075, 1520, 1568, 1568), (924, 1307));
    // High-resolution tier (2576 / 4784).
    assert_eq!(resized_size(1920, 1080, 2576, 4784), (1920, 1080));
    assert_eq!(resized_size(2000, 1500, 2576, 4784), (2000, 1500));
    assert_eq!(resized_size(3840, 2160, 2576, 4784), (2576, 1449));
}

/// A portrait image resolves through the landscape path with its axes swapped.
#[test]
fn resized_size_is_orientation_symmetric() {
    assert_eq!(resized_size(1080, 1920, 1568, 1568), (819, 1456));
    assert_eq!(resized_size(1520, 1075, 1568, 1568), (1307, 924));
}

#[test]
fn visual_token_arithmetic() {
    assert_eq!(visual_tokens(200, 200), 64);
    assert_eq!(visual_tokens(1000, 1000), 1296);
    assert_eq!(visual_tokens(1456, 819), 1560);
    assert_eq!(visual_tokens(2576, 1449), 4784);
    assert_eq!(visual_tokens(1, 1), 1);
}

fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let img = ImageBuffer::from_fn(width, height, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, 128u8])
    });
    let mut bytes = Vec::new();
    DynamicImage::ImageRgb8(img)
        .write_with_encoder(image::codecs::png::PngEncoder::new(&mut bytes))
        .unwrap();
    bytes
}

fn jpeg_bytes(width: u32, height: u32) -> Vec<u8> {
    let img = ImageBuffer::from_fn(width, height, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, 128u8])
    });
    let mut bytes = Vec::new();
    DynamicImage::ImageRgb8(img)
        .write_with_encoder(JpegEncoder::new_with_quality(&mut bytes, 90))
        .unwrap();
    bytes
}

/// An already-fitting API-native image passes through verbatim — the exact
/// input bytes, no recompression.
#[test]
fn fitting_native_bytes_pass_through_verbatim() {
    let png = png_bytes(100, 60);
    let prepared = prepare(&png, 1568, 1568).unwrap();
    assert_eq!(prepared.data, png);
    assert_eq!(prepared.media_type, "image/png");
    assert_eq!((prepared.width, prepared.height), (100, 60));
    assert_eq!(prepared.visual_tokens(), 4 * 3);
}

/// A resize re-encodes: non-JPEG input becomes lossless WebP at the target
/// size, and the produced bytes decode back to those dimensions.
#[test]
fn resized_png_reencodes_as_webp_at_target_size() {
    let png = png_bytes(100, 60);
    let prepared = prepare(&png, 56, 1568).unwrap();
    assert_eq!(prepared.media_type, "image/webp");
    assert_eq!((prepared.width, prepared.height), (56, 34));
    let roundtrip = image::load_from_memory(&prepared.data).unwrap();
    assert_eq!((roundtrip.width(), roundtrip.height()), (56, 34));
}

/// JPEG input stays JPEG through a resize — recompressing a photo as lossless
/// would balloon it.
#[test]
fn resized_jpeg_stays_jpeg() {
    let jpeg = jpeg_bytes(100, 60);
    let prepared = prepare(&jpeg, 56, 1568).unwrap();
    assert_eq!(prepared.media_type, "image/jpeg");
    assert_eq!((prepared.width, prepared.height), (56, 34));
    assert_eq!(
        image::guess_format(&prepared.data).unwrap(),
        ImageFormat::Jpeg
    );
}

/// Alpha survives a WebP re-encode.
#[test]
fn alpha_survives_reencode() {
    let img = ImageBuffer::from_fn(100, 60, |x, _| Rgba([255u8, 0, 0, (x % 256) as u8]));
    let mut png = Vec::new();
    DynamicImage::ImageRgba8(img)
        .write_with_encoder(image::codecs::png::PngEncoder::new(&mut png))
        .unwrap();
    let prepared = prepare(&png, 56, 1568).unwrap();
    assert_eq!(prepared.media_type, "image/webp");
    assert!(
        image::load_from_memory(&prepared.data)
            .unwrap()
            .color()
            .has_alpha()
    );
}

/// A region is cropped from the frame before sizing, so the limits bound the
/// crop; `Prepared` reports the frame it came from and the rectangle read.
#[test]
fn a_region_is_cropped_before_sizing() {
    let png = png_bytes(200, 200);
    let region = Crop {
        x: 10,
        y: 20,
        width: 50,
        height: 40,
    };
    let prepared = prepare_region(&png, Some(region), 56, 1568).unwrap();
    // The whole frame at max_edge 56 would be 56×56; the region fits as-is.
    assert_eq!((prepared.width, prepared.height), (50, 40));
    assert_eq!((prepared.source_width, prepared.source_height), (200, 200));
    assert_eq!(prepared.crop, Some(region));
    assert_eq!(prepared.pre_scale_size(), (50, 40));
    // A cropped read never passes through: the bytes are the region.
    assert_eq!(prepared.media_type, "image/webp");
}

/// A region overhanging the frame is clamped to the overlap; one with no
/// overlap is an error.
#[test]
fn region_clamping_and_the_empty_case() {
    let region = Crop {
        x: 80,
        y: 40,
        width: 50,
        height: 50,
    };
    assert_eq!(
        region.clamped_to(100, 60),
        Some(Crop {
            x: 80,
            y: 40,
            width: 20,
            height: 20
        })
    );
    assert_eq!(region.clamped_to(80, 60), None);
    assert_eq!(region.clamped_to(200, 200), Some(region));

    let png = png_bytes(100, 60);
    assert!(prepare_region(&png, Some(region), 1568, 1568).is_ok());
    let outside = Crop {
        x: 100,
        y: 0,
        width: 10,
        height: 10,
    };
    assert!(matches!(
        prepare_region(&png, Some(outside), 1568, 1568),
        Err(Error::RegionOutsideFrame(100, 60))
    ));
}

/// A non-image is a hard error, never a silent empty result.
#[test]
fn garbage_is_unrecognized() {
    assert!(matches!(
        prepare(b"not an image at all", 1568, 1568),
        Err(Error::Unrecognized)
    ));
}
