//! `efference devices`: how its verbs parse, the listing it prints, and the prefix
//! `revoke` resolves.

use super::*;
use efference::devices::{DeviceId, Joined, Kind};

fn device(id: &str, kind: Kind, label: &str, joined: Joined, last_seen: Option<i64>) -> Device {
    Device {
        id: DeviceId::parse_hex(id).expect("a test id"),
        kind,
        label: label.into(),
        joined,
        created_at: 1_000,
        last_seen_at: last_seen,
    }
}

fn enrolled() -> Vec<Device> {
    vec![
        device(
            "3f9a1c0b00000000000000000000000a",
            Kind::Browser,
            "Safari on macOS",
            Joined::Code,
            Some(1_000 + 2 * 86_400),
        ),
        device(
            "3f9a1c0b00000000000000000000000b",
            Kind::Script,
            "ci",
            Joined::Add,
            None,
        ),
        device(
            "a17e44d2000000000000000000000000",
            Kind::Script,
            "deploy",
            Joined::Add,
            Some(1_000 + 3 * 86_400 - 120),
        ),
    ]
}

#[test]
fn the_listing_is_aligned_with_the_label_last() {
    let now = 1_000 + 3 * 86_400;
    assert_eq!(
        listing(&enrolled(), now),
        "\
id        kind     joined  created  last seen  label
3f9a1c0b  browser  code    3d ago   1d ago     Safari on macOS
3f9a1c0b  script   add     3d ago   never      ci
a17e44d2  script   add     3d ago   2m ago     deploy
"
    );
}

#[test]
fn an_empty_listing_says_how_a_device_enrolls() {
    let empty = listing(&[], 0);
    assert!(empty.starts_with("No devices are enrolled."), "{empty}");
    assert!(empty.contains("efference devices add <label>"), "{empty}");
}

/// A revoke names one device by a prefix of its id, in either case; a prefix that
/// names none or several is refused with what there is to choose from.
#[test]
fn revoke_resolves_a_unique_prefix() {
    let devices = enrolled();
    assert_eq!(matching(&devices, "a17e").unwrap().label, "deploy");
    assert_eq!(matching(&devices, " A17E44D2 ").unwrap().label, "deploy");
    assert_eq!(
        matching(&devices, "3f9a1c0b00000000000000000000000b")
            .unwrap()
            .label,
        "ci"
    );

    let ambiguous = matching(&devices, "3f9a1c0b").unwrap_err().to_string();
    assert!(ambiguous.contains("give more of the id"), "{ambiguous}");
    assert!(
        ambiguous.contains("3f9a1c0b00000000000000000000000a (Safari on macOS)")
            && ambiguous.contains("3f9a1c0b00000000000000000000000b (ci)"),
        "{ambiguous}"
    );

    let unknown = matching(&devices, "ffff").unwrap_err().to_string();
    assert!(
        unknown.contains("3f9a1c0b (Safari on macOS), 3f9a1c0b (ci), a17e44d2 (deploy)"),
        "{unknown}"
    );
    assert!(
        matching(&devices, "").is_err(),
        "an empty prefix names nothing"
    );
    assert_eq!(
        matching(&[], "3f9a").unwrap_err().to_string(),
        "no devices are enrolled"
    );
}

/// `efference devices` lists with no verb; `add` needs the label and `revoke` the
/// id it names. `dev open` mints links only: a script's token is `devices add`.
#[test]
fn devices_lists_by_default_and_its_verbs_take_what_they_name() {
    use clap::Parser;
    let parse = |argv: &[&str]| Cli::try_parse_from(argv).and_then(Cli::checked);
    assert!(matches!(
        parse(&["efference", "devices"]).unwrap().command,
        Some(Command::Devices { command: None })
    ));
    assert!(matches!(
        parse(&["efference", "devices", "add", "ci"]).unwrap().command,
        Some(Command::Devices {
            command: Some(DevicesCommand::Add { label })
        }) if label == "ci"
    ));
    assert!(matches!(
        parse(&["efference", "devices", "revoke", "3f9a"]).unwrap().command,
        Some(Command::Devices {
            command: Some(DevicesCommand::Revoke { id })
        }) if id == "3f9a"
    ));
    for refused in [
        &["efference", "devices", "add"][..],
        &["efference", "devices", "revoke"],
        &["efference", "dev", "open", "--bearer"],
    ] {
        assert!(parse(refused).is_err(), "{refused:?}");
    }
}
