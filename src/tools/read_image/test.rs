use super::*;
use image::{DynamicImage, ImageBuffer, Rgb};

fn workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("eff-readimg-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_png(dir: &std::path::Path, name: &str, width: u32, height: u32) {
    let img = ImageBuffer::from_fn(width, height, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, 128u8])
    });
    let mut bytes = Vec::new();
    DynamicImage::ImageRgb8(img)
        .write_with_encoder(image::codecs::png::PngEncoder::new(&mut bytes))
        .unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
}

/// A relative path resolves against the tool's cwd; a fitting PNG passes
/// through, and the header states size, type, and visual-token cost.
#[test]
fn reads_a_fitting_png_by_relative_path() {
    let cwd = workspace("fit");
    write_png(&cwd, "shot.png", 100, 60);
    let tool = ReadImage { cwd };
    let result = tool.run(&json!({"path": "shot.png"}));
    assert!(!result.is_error, "got {:?}", result.content);
    assert_eq!(result.content, "100×60 image/png, 12 visual tokens");
    assert_eq!(result.images.len(), 1);
    let image = &result.images[0];
    assert_eq!(image.media_type, "image/png");
    assert_eq!((image.width, image.height), (100, 60));
    // The stored base64 round-trips to decodable bytes at the same dimensions.
    let bytes = STANDARD.decode(&image.data).unwrap();
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (100, 60));
}

/// `long_edge` caps the realized size; the header names the source dimensions
/// and the advice to read more.
#[test]
fn long_edge_downscales_and_the_header_says_so() {
    let cwd = workspace("cap");
    write_png(&cwd, "big.png", 100, 60);
    let tool = ReadImage { cwd };
    let result = tool.run(&json!({"path": "big.png", "long_edge": 56}));
    assert!(!result.is_error, "got {:?}", result.content);
    assert_eq!(
        result.content,
        "56×34 image/webp, 4 visual tokens \
         (downscaled 0.56× from 100×60; re-read with a larger long_edge for more detail)"
    );
    assert_eq!((result.images[0].width, result.images[0].height), (56, 34));
}

/// Which limit bound is derived by relaxing one limit at a time: the request's
/// `long_edge`, the token budget, or the tier's edge ceiling.
#[test]
fn constraint_that_bound_names_the_limit() {
    // Asked for less than the ceiling, and the ceiling would have given more.
    assert_eq!(constraint_that_bound(100, 60, 56), Bound::LongEdge);
    assert_eq!(constraint_that_bound(4000, 4000, 1568), Bound::LongEdge);
    // Square frames big enough to exceed 4784 patches while both edges fit 2576:
    // no long_edge helps.
    assert_eq!(constraint_that_bound(2400, 2400, 2576), Bound::TokenBudget);
    assert_eq!(constraint_that_bound(1960, 1960, 2576), Bound::TokenBudget);
    // A very wide strip runs out of edge with tokens to spare.
    assert_eq!(constraint_that_bound(6000, 100, 2576), Bound::EdgeCeiling);
}

/// At the ceiling the header names the token budget rather than advising a
/// larger `long_edge`, and points at the region read.
#[test]
fn a_budget_bound_read_says_so_and_points_at_a_region() {
    let cwd = workspace("budget");
    write_png(&cwd, "dense.png", 1960, 1960);
    let tool = ReadImage { cwd };
    let result = tool.run(&json!({"path": "dense.png", "long_edge": HIGH_RES_MAX_EDGE}));
    assert!(!result.is_error, "got {:?}", result.content);
    assert!(
        result
            .content
            .contains("by the visual-token budget (4784 tokens)"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("read a region for more detail"),
        "{}",
        result.content
    );
    assert!(
        !result.content.contains("larger long_edge"),
        "the unactionable hint must not survive: {}",
        result.content
    );
    assert!(result.images[0].width < 1960);
}

/// A strip that exhausts the 2576-px ceiling with tokens to spare names the
/// ceiling, not the budget — same "no larger long_edge" conclusion.
#[test]
fn an_edge_ceiling_read_names_the_ceiling() {
    let cwd = workspace("ceiling");
    write_png(&cwd, "strip.png", 6000, 100);
    let tool = ReadImage { cwd };
    let result = tool.run(&json!({"path": "strip.png", "long_edge": HIGH_RES_MAX_EDGE}));
    assert!(!result.is_error, "got {:?}", result.content);
    assert!(
        result.content.contains("by the 2576-px long_edge ceiling"),
        "{}",
        result.content
    );
    assert_eq!(
        (result.images[0].width, result.images[0].height),
        (2576, 43)
    );
}

/// A region reads only its rectangle, and the header echoes the crop origin
/// against the source frame so a follow-up can offset back to it.
#[test]
fn a_region_crops_and_the_header_echoes_the_origin() {
    let cwd = workspace("region");
    write_png(&cwd, "shot.png", 100, 60);
    let tool = ReadImage { cwd };
    let result = tool.run(&json!({
        "path": "shot.png",
        "region": {"x": 10, "y": 12, "w": 20, "h": 20}
    }));
    assert!(!result.is_error, "got {:?}", result.content);
    assert_eq!(
        result.content,
        "20×20 image/webp, 1 visual tokens (region 20×20 at 10,12 of 100×60; read 1:1)"
    );
    assert_eq!((result.images[0].width, result.images[0].height), (20, 20));
}

/// The crop precedes the sizing pass: a region that fits `long_edge` is read at
/// 1:1 even though the whole frame at the same `long_edge` would be downscaled.
#[test]
fn the_crop_precedes_the_resize() {
    let cwd = workspace("order");
    write_png(&cwd, "big.png", 200, 200);
    let tool = ReadImage { cwd };
    let whole = tool.run(&json!({"path": "big.png", "long_edge": 56}));
    assert_eq!((whole.images[0].width, whole.images[0].height), (56, 56));
    let quarter = tool.run(&json!({
        "path": "big.png",
        "long_edge": 56,
        "region": {"x": 0, "y": 0, "w": 50, "h": 50}
    }));
    assert!(!quarter.is_error, "got {:?}", quarter.content);
    assert_eq!(
        (quarter.images[0].width, quarter.images[0].height),
        (50, 50)
    );
    assert!(quarter.content.contains("read 1:1"), "{}", quarter.content);
}

/// A region extending past the frame is clamped rather than refused — the model
/// addresses from coordinates it remembers, and the header states the rectangle
/// it actually got.
#[test]
fn a_partly_out_of_bounds_region_is_clamped_with_the_header_saying_so() {
    let cwd = workspace("clamp");
    write_png(&cwd, "shot.png", 100, 60);
    let tool = ReadImage { cwd };
    let result = tool.run(&json!({
        "path": "shot.png",
        "region": {"x": 80, "y": 40, "w": 50, "h": 50}
    }));
    assert!(!result.is_error, "got {:?}", result.content);
    assert_eq!(
        result.content,
        "20×20 image/webp, 1 visual tokens \
         (region 20×20 at 80,40 of 100×60, clamped to the frame; read 1:1)"
    );
}

/// A region with no cogent reading — zero area, or wholly outside the frame —
/// is an error, not a clamp.
#[test]
fn an_empty_or_outside_region_is_an_error() {
    let cwd = workspace("badregion");
    write_png(&cwd, "shot.png", 100, 60);
    let tool = ReadImage { cwd };

    let zero = tool.run(&json!({"path": "shot.png", "region": {"x": 0, "y": 0, "w": 0, "h": 10}}));
    assert!(zero.is_error);
    assert!(zero.content.contains("zero area"), "{}", zero.content);
    assert!(zero.images.is_empty());

    let outside =
        tool.run(&json!({"path": "shot.png", "region": {"x": 200, "y": 0, "w": 10, "h": 10}}));
    assert!(outside.is_error);
    assert!(
        outside
            .content
            .contains("does not overlap the 100×60 image"),
        "{}",
        outside.content
    );

    // The far edge exactly on the boundary leaves nothing inside either.
    let flush =
        tool.run(&json!({"path": "shot.png", "region": {"x": 0, "y": 60, "w": 10, "h": 10}}));
    assert!(flush.is_error, "{}", flush.content);
}

/// A malformed `region` is a request error before any file is read.
#[test]
fn a_malformed_region_is_an_error() {
    let cwd = workspace("regionshape");
    write_png(&cwd, "shot.png", 100, 60);
    let tool = ReadImage { cwd };
    for region in [
        json!([0, 0, 10, 10]),
        json!({"x": 0, "y": 0, "w": 10}),
        json!({"x": 0, "y": 0, "w": "10", "h": 10}),
        json!({"x": -5, "y": 0, "w": 10, "h": 10}),
    ] {
        let result = tool.run(&json!({"path": "shot.png", "region": region.clone()}));
        assert!(result.is_error, "{region} produced {}", result.content);
        assert!(result.images.is_empty());
    }
}

#[test]
fn missing_file_is_an_error_result_with_no_image() {
    let tool = ReadImage {
        cwd: workspace("missing"),
    };
    let result = tool.run(&json!({"path": "nope.png"}));
    assert!(result.is_error);
    assert!(result.images.is_empty());
    assert!(result.content.contains("nope.png"), "{}", result.content);
}

#[test]
fn a_non_image_file_is_an_error_result() {
    let cwd = workspace("text");
    std::fs::write(cwd.join("notes.txt"), "just prose").unwrap();
    let tool = ReadImage { cwd };
    let result = tool.run(&json!({"path": "notes.txt"}));
    assert!(result.is_error);
    assert!(
        result.content.contains("unrecognized image format"),
        "{}",
        result.content
    );
}

#[test]
fn malformed_input_is_an_error_result() {
    let tool = ReadImage {
        cwd: workspace("malformed"),
    };
    assert!(tool.run(&json!({})).is_error);
    assert!(
        tool.run(&json!({"path": "x.png", "long_edge": "big"}))
            .is_error
    );
}
