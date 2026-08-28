//! Tests for the attachment store: naming, classification, and the notification body.

use super::*;

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("efference-att-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A colliding name is suffixed, never overwritten: an attachment the model may not have read
/// yet is not something a later arrival gets to replace.
#[test]
fn store_makes_names_unique_instead_of_overwriting() {
    let dir = tmpdir("unique");
    let first = store(&dir, "paste.txt", b"one").unwrap();
    let second = store(&dir, "paste.txt", b"two").unwrap();
    assert_eq!(first.file_name().unwrap(), "paste.txt");
    assert_eq!(second.file_name().unwrap(), "paste-2.txt");
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "one");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A suggested name is reduced to one path component — nothing may escape the store.
#[test]
fn store_names_cannot_escape_the_directory() {
    let dir = tmpdir("escape");
    let path = store(&dir, "../../etc/passwd", b"x").unwrap();
    assert_eq!(path.parent().unwrap(), dir);
    assert_eq!(path.file_name().unwrap(), "-..-etc-passwd");
    let path = store(&dir, "   ", b"x").unwrap();
    assert_eq!(path.file_name().unwrap(), "attachment");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Text classification carries the triage signals the notice states: size, line count, the
/// sniffed shape, and a first line.
#[test]
fn text_is_classified_with_shape_and_first_line() {
    let dir = tmpdir("text");
    let path = store(
        &dir,
        "log.txt",
        b"\n2026-08-27 ERROR boom\n2026-08-27 INFO ok\n",
    )
    .unwrap();
    let arrival = inspect("attachments/log.txt", &path).unwrap();
    let Kind::Text {
        lines,
        shape,
        first_line,
        ..
    } = arrival.kind
    else {
        panic!("expected text, got {:?}", arrival.kind)
    };
    assert_eq!(lines, 3);
    assert_eq!(shape, Shape::Log);
    assert_eq!(first_line, "2026-08-27 ERROR boom");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn shapes_are_sniffed_from_the_head() {
    assert_eq!(sniff_shape("{\n \"a\": 1\n}\n"), Shape::Json);
    assert_eq!(
        sniff_shape("diff --git a/x b/x\n@@ -1 +1 @@\n"),
        Shape::Diff
    );
    assert_eq!(
        sniff_shape("thread 'main' panicked at src/x.rs:1\n"),
        Shape::Stacktrace
    );
    assert_eq!(
        sniff_shape("Traceback (most recent call last)\n  File \"x\"\n"),
        Shape::Stacktrace
    );
    assert_eq!(sniff_shape("Hello there, a note.\n"), Shape::Prose);
}

/// A real PNG classifies as an image with its dimensions — the signal that decides whether an
/// image is worth spending a look on.
#[test]
fn an_image_is_classified_with_its_dimensions() {
    let dir = tmpdir("image");
    let mut png = Vec::new();
    let img = image::RgbaImage::new(7, 3);
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let path = store(&dir, "shot.png", &png).unwrap();
    let arrival = inspect("attachments/shot.png", &path).unwrap();
    let Kind::Image { width, height, .. } = arrival.kind else {
        panic!("expected an image, got {:?}", arrival.kind)
    };
    assert_eq!((width, height), (7, 3));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The notification names what arrived and where, and takes no stance on what to do with it —
/// the harness is neutral about disposition (docs/inbox.md). The slot is the path, so a second
/// arrival at the same name supersedes the first notice.
#[test]
fn the_notification_states_what_arrived_and_nothing_else() {
    let arrival = Arrival {
        path: ".efference/s1/attachments/paste.txt".into(),
        kind: Kind::Text {
            bytes: 2048,
            lines: 40,
            shape: Shape::Json,
            first_line: "{\"a\": 1,".into(),
        },
    };
    let (source, body, slot) = notification(&arrival);
    assert_eq!(source, "attachment");
    assert_eq!(
        body,
        "attachment at .efference/s1/attachments/paste.txt\n\
         text · 2.0 KB · 40 lines · looks like JSON\n\
         first line: {\"a\": 1,"
    );
    assert_eq!(
        slot.as_deref(),
        Some("attachment:.efference/s1/attachments/paste.txt")
    );
}
