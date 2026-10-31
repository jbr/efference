use super::*;
use crate::log::attachment::Kind;

fn arrival(name: &str) -> Arrival {
    Arrival {
        name: name.into(),
        path: format!("att/composer/{name}"),
        kind: Kind::Binary { bytes: 1 },
    }
}

/// A chip is ready once its upload lands, and a send carries only ready ones.
#[test]
fn a_landed_upload_is_ready_to_send() {
    let mut staged = StagedAttachments::default();
    let AttachCmd::Upload { key, .. } = staged.stage("a.png".into(), vec![1]) else {
        panic!("staging uploads")
    };
    assert!(staged.uploading());
    assert!(staged.ready().is_empty());
    assert_eq!(staged.landed(key, Ok(arrival("a.png"))), None);
    assert!(!staged.uploading());
    assert_eq!(staged.ready(), vec![arrival("a.png")]);
}

/// Removing a chip mid-upload removes what the upload stored when it lands — never a chip
/// that reappears, and never an orphan left in the store.
#[test]
fn a_chip_removed_mid_upload_removes_its_upload() {
    let mut staged = StagedAttachments::default();
    let AttachCmd::Upload { key, .. } = staged.stage("a.png".into(), vec![1]) else {
        panic!("staging uploads")
    };
    assert_eq!(staged.remove(key), None);
    assert!(staged.is_empty());
    assert_eq!(
        staged.landed(key, Ok(arrival("a-2.png"))),
        Some(AttachCmd::Remove {
            name: "a-2.png".into()
        })
    );
    assert!(staged.is_empty());
}

/// A ready chip's removal names the stored file.
#[test]
fn removing_a_ready_chip_removes_the_stored_file() {
    let mut staged = StagedAttachments::default();
    staged.restore(vec![arrival("r.txt")]);
    let key = staged.last_key().unwrap();
    assert_eq!(
        staged.remove(key),
        Some(AttachCmd::Remove {
            name: "r.txt".into()
        })
    );
}

/// The shapes terminals type on a drop — backslash-escaped, quoted, `file://` — each resolve
/// to the file; anything that is not wholly existing absolute paths is text.
#[test]
fn a_drop_is_recognized_by_its_paths() {
    let dir = std::env::temp_dir().join(format!("efference-drop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let spaced = dir.join("My Shot.png");
    let plain = dir.join("notes.txt");
    std::fs::write(&spaced, b"x").unwrap();
    std::fs::write(&plain, b"x").unwrap();
    let escaped = spaced.display().to_string().replace(' ', "\\ ");

    assert_eq!(dropped_paths(&escaped), Some(vec![spaced.clone()]));
    assert_eq!(
        dropped_paths(&format!("'{}' {} ", spaced.display(), plain.display())),
        Some(vec![spaced.clone(), plain.clone()])
    );
    let url = format!(
        "file://{}",
        spaced.display().to_string().replace(' ', "%20")
    );
    assert_eq!(dropped_paths(&url), Some(vec![spaced.clone()]));

    // Text that merely mentions a path, a missing file, a relative path, a directory.
    assert_eq!(dropped_paths(&format!("see {}", plain.display())), None);
    assert_eq!(dropped_paths(&format!("{}/missing", dir.display())), None);
    assert_eq!(dropped_paths("Cargo.toml"), None);
    assert_eq!(dropped_paths(&dir.display().to_string()), None);
    assert_eq!(dropped_paths(""), None);
    assert_eq!(dropped_paths("'unterminated"), None);
    std::fs::remove_dir_all(&dir).unwrap();
}
