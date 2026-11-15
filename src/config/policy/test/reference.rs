//! Admission's additive path grants (docs/permissions.md): what a submitted document's
//! filesystem references ask for, what the policy already covers, and the ways a batch
//! of references stays additive as a whole.

use super::*;
use crate::record::log::{
    ConfigChange, ReferenceAccess, ReferenceDefinition, ReferenceTargetKind, UserContent,
    UserSegment,
};
use std::collections::BTreeMap;

fn dirs() -> Directories {
    Directories::at(Path::new(CWD))
}

const READ: ReferenceAccess = ReferenceAccess {
    read: true,
    write: false,
};
const WRITE: ReferenceAccess = ReferenceAccess {
    read: true,
    write: true,
};

/// A document whose segments are one reference per entry:
/// `(id, what was typed, absolute path, kind, requested access)`.
fn document(refs: &[(&str, &str, &str, ReferenceTargetKind, ReferenceAccess)]) -> UserContent {
    UserContent {
        content: refs
            .iter()
            .map(|(id, ..)| UserSegment::Reference((*id).to_string()))
            .collect(),
        references: refs
            .iter()
            .map(|(id, original, path, kind, access)| {
                (
                    (*id).to_string(),
                    ReferenceDefinition::filesystem(*id, *original, *path, *kind, *access),
                )
            })
            .collect::<BTreeMap<_, _>>(),
    }
}

/// The grants, asserting every refusal-free document produced exactly the expected keys.
fn granted(document: &UserContent, role: &Config) -> Vec<(String, ReferenceAccess)> {
    let (grants, unusable) = filesystem_grants(document, role, &dirs());
    assert!(unusable.is_empty(), "no target is unusable here");
    grants
        .into_iter()
        .map(|grant| {
            let ConfigChange::Path { path, read, write } = &grant.change else {
                panic!("a reference grants a path");
            };
            assert_eq!(
                path, &grant.key,
                "the change names the key it is written at"
            );
            (
                grant.key.clone(),
                ReferenceAccess {
                    read: *read,
                    write: *write,
                },
            )
        })
        .collect()
}

/// The policy after the batch is folded in the order it was emitted — what the log will
/// do — so an assertion is about the policy the session runs under, not the grant list.
fn folded(role: &Config, document: &UserContent) -> Config {
    let (grants, _) = filesystem_grants(document, role, &dirs());
    let mut role = role.clone();
    for grant in grants {
        apply_config_change(&mut role, &grant.change);
    }
    role
}

fn granted_at(role: &Config, path: &str, perm: Perm) -> bool {
    path_granted_at(role, &dirs().judged(Path::new(path)), perm, &dirs())
}

#[test]
fn a_reference_grants_read_at_its_target() {
    let empty = ConfigFixture::new().build();
    let doc = document(&[(
        "a",
        "notes.md",
        "/work/notes.md",
        ReferenceTargetKind::File,
        READ,
    )]);
    assert!(has_filesystem_references(&doc));
    assert_eq!(
        granted(&doc, &empty),
        [("notes.md".to_string(), READ)],
        "a read reference is a read-only grant at the file, keyed project-relative"
    );
    // Outside the project the key stays absolute, which `[paths]` already accepts.
    let outside = document(&[(
        "a",
        "/elsewhere/report.md",
        "/elsewhere/report.md",
        ReferenceTargetKind::File,
        READ,
    )]);
    assert_eq!(
        granted(&outside, &empty),
        [("/elsewhere/report.md".to_string(), READ)]
    );
}

#[test]
fn a_write_reference_grants_read_with_it() {
    let empty = ConfigFixture::new().build();
    let doc = document(&[(
        "a",
        "src/",
        "/work/src",
        ReferenceTargetKind::Directory,
        WRITE,
    )]);
    assert_eq!(granted(&doc, &empty), [("src".to_string(), WRITE)]);
    assert!(granted_at(
        &folded(&empty, &doc),
        "/work/src/main.rs",
        Perm::Write
    ));
}

#[test]
fn a_document_without_filesystem_references_grants_nothing() {
    use crate::record::log::attachment::{self, Arrival};

    let empty = ConfigFixture::new().build();
    let plain = UserContent::text("just prose");
    assert!(!has_filesystem_references(&plain));
    assert!(granted(&plain, &empty).is_empty());

    let attached = UserContent {
        content: vec![
            UserSegment::Text("look ".into()),
            UserSegment::Reference("a".into()),
        ],
        references: BTreeMap::from([(
            "a".into(),
            ReferenceDefinition::attachment(Arrival {
                name: "notes.txt".into(),
                path: "att/composer/notes.txt".into(),
                kind: attachment::Kind::Binary { bytes: 3 },
            }),
        )]),
    };
    assert!(
        !has_filesystem_references(&attached),
        "an upload is not a filesystem target: its bytes are already in the store"
    );
    assert!(granted(&attached, &empty).is_empty());
}

#[test]
fn a_covered_target_grants_nothing() {
    // Covered by a broader `[paths]` grant: no exact key is needed.
    let broad = ConfigFixture::new().with_path(".", true, false).build();
    let doc = document(&[(
        "a",
        "src/main.rs",
        "/work/src/main.rs",
        ReferenceTargetKind::File,
        READ,
    )]);
    assert!(granted(&doc, &broad).is_empty());
    // Coverage is asked of the judged target, so a key spelled another way still covers it.
    let respelled = ConfigFixture::new()
        .with_path("/work/src/main.rs", true, false)
        .build();
    assert!(granted(&doc, &respelled).is_empty());
    // Write is not covered by a read grant, so the same target does need a grant.
    let write_doc = document(&[(
        "a",
        "src/main.rs",
        "/work/src/main.rs",
        ReferenceTargetKind::File,
        WRITE,
    )]);
    assert_eq!(
        granted(&write_doc, &broad),
        [("src/main.rs".to_string(), WRITE)],
        "the grant adds write without taking the broader read away"
    );
    assert!(granted_at(
        &folded(&broad, &write_doc),
        "/work/src/other.rs",
        Perm::Read
    ));
    assert!(!granted_at(
        &folded(&broad, &write_doc),
        "/work/src/other.rs",
        Perm::Write
    ));
}

#[test]
fn two_references_to_one_target_combine() {
    let empty = ConfigFixture::new().build();
    let doc = document(&[
        (
            "a",
            "src",
            "/work/src",
            ReferenceTargetKind::Directory,
            READ,
        ),
        (
            "b",
            "src",
            "/work/src",
            ReferenceTargetKind::Directory,
            WRITE,
        ),
    ]);
    assert_eq!(
        granted(&doc, &empty),
        [("src".to_string(), WRITE)],
        "read and write chips on one directory ask for read+write once"
    );
}

/// The batch is additive as a whole: a write on a directory admits a read reference to a
/// file inside it. Computed independently against the same role, the child would be
/// granted a read-only key that folds *after* the directory and takes its write away for
/// that one file.
#[test]
fn a_child_read_does_not_carve_write_out_of_a_new_parent_grant() {
    let empty = ConfigFixture::new().build();
    let doc = document(&[
        ("dir", "a", "/work/a", ReferenceTargetKind::Directory, WRITE),
        (
            "file",
            "a/b.txt",
            "/work/a/b.txt",
            ReferenceTargetKind::File,
            READ,
        ),
    ]);
    assert_eq!(
        granted(&doc, &empty),
        [("a".to_string(), WRITE)],
        "the covered child adds no key at all, so nothing can narrow the directory grant"
    );
    let effective = folded(&empty, &doc);
    assert!(granted_at(&effective, "/work/a/b.txt", Perm::Write));
}

#[test]
fn a_child_write_under_a_new_parent_read_needs_its_own_key() {
    let empty = ConfigFixture::new().build();
    let doc = document(&[
        ("dir", "a", "/work/a", ReferenceTargetKind::Directory, READ),
        (
            "file",
            "a/b.txt",
            "/work/a/b.txt",
            ReferenceTargetKind::File,
            WRITE,
        ),
    ]);
    assert_eq!(
        granted(&doc, &empty),
        [("a".to_string(), READ), ("a/b.txt".to_string(), WRITE)],
        "the child's write is not covered, and its key is deeper than the directory's"
    );
    let effective = folded(&empty, &doc);
    assert!(granted_at(&effective, "/work/a/b.txt", Perm::Write));
    // The deeper key adds for that file and leaves the directory's grant for its siblings.
    assert!(granted_at(&effective, "/work/a/other.txt", Perm::Read));
    assert!(!granted_at(&effective, "/work/a/other.txt", Perm::Write));
}

#[test]
fn a_deeper_deny_is_not_coverage_and_survives_the_grant() {
    let role = ConfigFixture::new()
        .with_path(".", true, true)
        .with_path("src/secret", false, false)
        .build();
    // A reference inside the deny: the deny governs, so it is not coverage.
    let doc = document(&[(
        "a",
        "src/secret/keys",
        "/work/src/secret/keys",
        ReferenceTargetKind::Directory,
        READ,
    )]);
    assert_eq!(
        granted(&doc, &role),
        [("src/secret/keys".to_string(), READ)],
        "the grant is written below the deny, not over it"
    );
    let effective = folded(&role, &doc);
    assert!(granted_at(
        &effective,
        "/work/src/secret/keys/id_rsa",
        Perm::Read
    ));
    // The deny still governs the rest of its subtree.
    assert!(!granted_at(
        &effective,
        "/work/src/secret/other",
        Perm::Read
    ));
    // A reference to a parent the deny sits under adds nothing: the broad grant covers it.
    let covered = document(&[(
        "a",
        "src",
        "/work/src",
        ReferenceTargetKind::Directory,
        WRITE,
    )]);
    assert!(granted(&covered, &role).is_empty());
}

/// The session's own `.efference` surface is injected at equal specificity; a grant the
/// reference writes there is deeper than everything but the root itself, and at the root
/// it is written later in the fold, so either way the reference is what governs.
#[test]
fn an_equal_specificity_deny_is_overridden_by_the_reference() {
    let role = ConfigFixture::new()
        .with_path(".", true, true)
        .with_path(".efference", false, false)
        .build();
    let doc = document(&[(
        "a",
        ".efference/log.jsonl",
        "/work/.efference/log.jsonl",
        ReferenceTargetKind::File,
        READ,
    )]);
    assert_eq!(
        granted(&doc, &role),
        [(".efference/log.jsonl".to_string(), READ)]
    );
    // The root itself: equal specificity, so the fold order decides — last key wins.
    let root = document(&[(
        "a",
        ".efference",
        "/work/.efference",
        ReferenceTargetKind::Directory,
        READ,
    )]);
    assert_eq!(
        granted(&root, &role),
        [(".efference".to_string(), READ)],
        "the reference key is written at the same specificity as the injected deny"
    );
    let effective = folded(&role, &root);
    assert!(granted_at(
        &effective,
        "/work/.efference/log.jsonl",
        Perm::Read
    ));
}

#[test]
fn grant_keys_are_project_relative_and_absolute_outside() {
    let dirs = dirs();
    assert_eq!(dirs.grant_key(Path::new("/work")).as_deref(), Some("."));
    assert_eq!(
        dirs.grant_key(Path::new("/work/src/a.rs")).as_deref(),
        Some("src/a.rs")
    );
    assert_eq!(
        dirs.grant_key(Path::new("/elsewhere/a")).as_deref(),
        Some("/elsewhere/a")
    );
    assert_eq!(
        dirs.grant_key(Path::new("/workshop/a")).as_deref(),
        Some("/workshop/a")
    );
}

#[test]
fn a_worktree_target_is_keyed_at_its_project_counterpart() {
    let project = Path::new("/work");
    let dirs = Directories::new(
        project.to_path_buf(),
        project.join(".efference/worktrees/w1"),
    );
    let target = dirs.judged(Path::new("/work/.efference/worktrees/w1/src/a.rs"));
    assert_eq!(target, Path::new("/work/src/a.rs"));
    assert_eq!(dirs.grant_key(&target).as_deref(), Some("src/a.rs"));
    // A document naming the worktree path grants the project key, for every session.
    let doc = document(&[(
        "a",
        "src/a.rs",
        "/work/.efference/worktrees/w1/src/a.rs",
        ReferenceTargetKind::File,
        READ,
    )]);
    let (grants, _) = filesystem_grants(&doc, &ConfigFixture::new().build(), &dirs);
    assert_eq!(grants[0].key, "src/a.rs");
}

#[test]
fn a_target_without_a_utf8_spelling_is_refused() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let target = PathBuf::from(OsString::from_vec(b"/work/\xff".to_vec()));
    assert_eq!(dirs().grant_key(&target), None);

    let doc = document(&[(
        "a",
        "typed",
        "/work/\u{fffd}",
        ReferenceTargetKind::File,
        READ,
    )]);
    // A replacement character is valid UTF-8, so it keys; the check above is the
    // non-UTF-8 refusal. This pins that the two are not conflated.
    assert!(
        granted(&doc, &ConfigFixture::new().build())[0]
            .0
            .ends_with('\u{fffd}')
    );
}
/// A grant is founded on the absolute target, and its key is a shortened spelling of it when the
/// target is inside the project. Both are kept: admission's mask probe checks the key and the
/// target separately, because an `[env]` needle can match the absolute path while the relative
/// key is untouched (`session/grants.rs`).
#[test]
fn a_grant_keeps_the_target_its_key_shortens() {
    let doc = document(&[(
        "a",
        "secret/notes.md",
        "/work/secret/notes.md",
        ReferenceTargetKind::File,
        READ,
    )]);
    let (grants, _) = filesystem_grants(&doc, &Config::default(), &dirs());
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].key, "secret/notes.md");
    assert_eq!(
        grants[0].target,
        PathBuf::from("/work/secret/notes.md"),
        "the target keeps the absolute spelling the key shortens"
    );
}
