//! Staged attachments as the operator sees them (docs/tui.md): a chip per staged file above
//! the control strip, removable by its `✕` or by `⌫` on an empty composer, sent with the next
//! turn and shown under it in the transcript.

use super::*;
use crate::attachments::{Arrival, Kind};
use crate::tui::staged::AttachCmd;

fn shot() -> Arrival {
    Arrival {
        name: "shot.png".into(),
        path: ".efference/s/attachments/composer/shot.png".into(),
        kind: Kind::Image {
            format: "PNG".into(),
            width: 1280,
            height: 800,
            bytes: 245_760,
        },
    }
}

/// Stage a long paste and land its upload, returning the chip's key.
fn stage_and_land(play: &mut Screenplay<'_>, arrival: Arrival) -> u64 {
    let long: String = (0..30).map(|n| format!("line {n}\n")).collect();
    play.state.handle_paste(long);
    let Some(AttachCmd::Upload { key, .. }) = play.took_attach() else {
        panic!("a long paste uploads")
    };
    play.event(TuiEvent::Uploaded {
        key,
        result: Ok(arrival),
    });
    key
}

#[test]
fn a_staged_chip_shows_above_the_strip_and_its_cross_removes_it() {
    let mut play = Screenplay::new(70, 12);
    stage_and_land(&mut play, shot());
    play.snapshot("staged_attachment_row");

    let (x, y) = play.find("✕").expect("the chip has a remove control");
    play.click(x, y);
    assert!(play.find("shot.png").is_none(), "the chip is gone");
    assert_eq!(
        play.took_attach(),
        Some(AttachCmd::Remove {
            name: "shot.png".into()
        }),
        "the upload is removed from the store"
    );
}

/// `⌫` on an empty composer takes back the last chip; with text in the composer it edits
/// the text as always.
#[test]
fn backspace_on_an_empty_composer_takes_back_the_last_chip() {
    let mut play = Screenplay::new(70, 12);
    stage_and_land(&mut play, shot());
    play.type_str("x");
    play.route_key(KeyCode::Backspace, KeyModifiers::NONE);
    assert!(play.find("shot.png").is_some(), "the text went first");
    play.route_key(KeyCode::Backspace, KeyModifiers::NONE);
    assert!(play.find("shot.png").is_none(), "then the chip");
}

/// Attachments alone are a turn: `⏎` on an empty composer sends them rather than taking the
/// continue, and the pending echo names what it carried.
#[test]
fn an_empty_composer_with_chips_sends_them() {
    let mut play = Screenplay::new(70, 12);
    stage_and_land(&mut play, shot());
    play.route_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(!play.took_continue(), "not the zero-token continue");
    let sent = play.took_submission().expect("a turn was sent");
    assert_eq!(sent.turn.attachments, vec![shot()]);
    assert!(sent.turn.text.is_empty());
    play.snapshot("attachment_turn_pending");
}

/// On a terminal that draws images, a staged image shows as a thumbnail in a band above its
/// chip; removing the chip takes the band away with it.
#[test]
fn a_staged_image_shows_its_thumbnail() {
    let mut play = Screenplay::new(70, 16);
    play.state
        .staged_images
        .set_picker(Some(ratatui_image::picker::Picker::halfblocks()));
    let key = stage_and_land(&mut play, shot());
    let image = ::image::DynamicImage::ImageRgba8(::image::RgbaImage::from_pixel(
        640,
        400,
        ::image::Rgba([200, 60, 120, 255]),
    ));
    play.event(TuiEvent::Thumbnail { key, image });
    play.snapshot("staged_attachment_thumbnail");
    // The grid drops color; the thumbnail's cells carry the image's.
    let (_, chip_row) = play.find("shot.png").unwrap();
    let pixel = play.cell(2, chip_row - 1);
    assert!(
        [pixel.fg, pixel.bg].contains(&Color::Rgb(200, 60, 120)),
        "the band above the chip is the image: {pixel:?}"
    );

    let (x, y) = play.find("✕").unwrap();
    play.click(x, y);
    assert_eq!(play.state.staged_rows(), 0, "the band goes with its chip");
}

/// A terminal's own paste cannot carry an image, so the strip says when the clipboard holds
/// one (by its type list — the watch never reads it), and a change that leaves none takes
/// the offer down.
#[test]
fn an_image_on_the_clipboard_is_offered_on_the_strip() {
    let mut play = Screenplay::new(80, 8);
    play.event(TuiEvent::ClipboardImage(true));
    assert!(play.find("image in clipboard · C-v to attach").is_some());
    play.event(TuiEvent::ClipboardImage(false));
    assert!(play.find("image in clipboard").is_none());
}
