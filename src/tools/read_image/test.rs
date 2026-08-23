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
/// and the escalation path.
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
         (downscaled from 100×60; re-read with a larger long_edge for more detail)"
    );
    assert_eq!((result.images[0].width, result.images[0].height), (56, 34));
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
