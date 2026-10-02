//! Result images (docs/tui.md): the reserved rectangle, the halfblocks render,
//! the pending and off states, and the partial-scroll row slice.

use super::*;
use crate::log::ResultImage;

/// A deterministic gradient PNG, carried inline the way an offline log's
/// entries carry their bytes.
fn png_image(width: u32, height: u32) -> ResultImage {
    use base64::Engine;
    let img =
        ::image::DynamicImage::ImageRgba8(::image::RgbaImage::from_fn(width, height, |x, y| {
            ::image::Rgba([(x % 256) as u8, ((x + y) % 256) as u8, (y % 256) as u8, 255])
        }));
    let mut bytes = std::io::Cursor::new(Vec::new());
    img.write_to(&mut bytes, ::image::ImageFormat::Png).unwrap();
    ResultImage {
        media_type: "image/png".into(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),
        width,
        height,
    }
}

/// The same shape as the live entry view: metadata only, bytes a fetch away.
fn png_reference(width: u32, height: u32) -> ResultImage {
    ResultImage {
        media_type: "image/jpeg".into(),
        data: String::new(),
        width,
        height,
    }
}

fn image_scenario(image: ResultImage) -> Screenplay<'static> {
    image_scenario_sized(80, 36, image)
}

fn image_scenario_sized(width: u16, height: u16, image: ResultImage) -> Screenplay<'static> {
    let mut play = Screenplay::new(width, height);
    play.halfblock_images();
    play.user("read the fixture image");
    play.assistant(
        "",
        vec![ToolCall::new(
            "read_image",
            "toolu_img",
            json!({ "path": "earthrise.jpg" }),
        )],
    );
    play.tool_result_images(
        "toolu_img",
        "600×400 image/png, 900 visual tokens",
        vec![image],
    );
    play
}

/// Inline bytes render as the halfblock mosaic, inside the result entry, with
/// the header counting the image beside the text size.
#[test]
fn transcript_image_result() {
    let mut play = image_scenario(png_image(600, 400));
    play.scroll_to_top();
    play.snapshot("transcript_image_result");
}

/// A reference whose bytes haven't arrived reserves its full rectangle and says
/// what it's waiting for — the placeholder the live path shows between the
/// entry landing and the fetch answering.
#[test]
fn transcript_image_pending() {
    let mut play = image_scenario(png_reference(1568, 1568));
    play.scroll_to_top();
    play.snapshot("transcript_image_pending");
}

/// With images off (no picker), the image renders as its one-line reference —
/// no blank reservation.
#[test]
fn transcript_image_off() {
    let mut play = Screenplay::new(80, 24);
    play.user("read the fixture image");
    play.assistant(
        "",
        vec![ToolCall::new(
            "read_image",
            "toolu_img",
            json!({ "path": "earthrise.jpg" }),
        )],
    );
    play.tool_result_images(
        "toolu_img",
        "600×400 image/png, 900 visual tokens",
        vec![png_image(600, 400)],
    );
    play.scroll_to_top();
    play.snapshot("transcript_image_off");
}

/// Scrolling an image partly off the top keeps its reservation and paints the
/// visible row slice — the halfblocks blit, which the pixel protocols can't do
/// (docs/tui.md). The top viewport row is mid-image.
#[test]
fn transcript_image_partial_scroll() {
    // A viewport much shorter than the document, plus content below the image,
    // so an offset that cuts the image's top survives scroll clamping.
    let mut play = image_scenario_sized(80, 16, png_image(600, 400));
    play.user("thanks — that matches the fixture");
    play.state.viewport.stuck_to_bottom = false;
    play.render(); // settle the layout so the offset survives clamping
    // The mosaic occupies document rows ~15–34; land the viewport mid-image.
    play.state.set_scroll_offset(24);
    let (_, y) = play.find("▄").expect("a mosaic slice is on screen");
    assert_eq!(
        y, play.state.viewport.doc_origin.y,
        "the top visible row is mid-image, not the image's top"
    );
    play.snapshot("transcript_image_partial_scroll");
}

/// The no-reflow invariant (docs/tui.md): the document is exactly as tall
/// while the bytes are in flight as after they land, so arrival repaints the
/// reserved rows and moves nothing.
#[test]
fn image_reservation_is_stable_across_byte_arrival() {
    let height = |play: &mut Screenplay<'static>| {
        play.render();
        play.state
            .viewport
            .cached_layout
            .expect("rendered")
            .document_height
    };
    let mut pending = image_scenario(png_reference(600, 400));
    let mut landed = image_scenario(png_image(600, 400));
    assert_eq!(height(&mut pending), height(&mut landed));
}
