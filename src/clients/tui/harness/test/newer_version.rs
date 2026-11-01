//! A newer release in the status bar (docs/tui.md): stated at the right edge while the
//! `newer_version` topic names one, and dropped where the line has no room for it.

use super::*;
use crate::clients::tui::topics::TopicUpdate;
use crate::protocol::state::NewerVersion;

fn newer(version: Option<&str>) -> TuiEvent {
    TuiEvent::Topic(TopicUpdate::NewerVersion(version.map(|version| {
        NewerVersion {
            version: version.to_string(),
            tag: format!("v{version}"),
            url: format!("https://efference.net/releases/v{version}"),
            update_command: "curl -fsSL https://efference.net/install.sh | sh".into(),
        }
    })))
}

/// The notice sits on the status bar's row, flush right, dimmed, and leaves when
/// the topic clears.
#[test]
fn a_newer_release_is_stated_at_the_right_of_the_status_bar() {
    let mut play = Screenplay::new(100, 14);
    play.user("Hello.");
    assert!(!play.render().contains("is available"));

    play.event(newer(Some("0.2.0")));
    let notice = "efference 0.2.0 is available";
    let (x, y) = play.find(notice).expect("the notice is drawn");
    assert_eq!(y, 13, "on the status bar, the last row");
    assert_eq!(
        x + notice.chars().count() as u16,
        100 - crate::clients::tui::render_document::BASELINE_LEFT_MARGIN,
        "flush right, inside a margin as wide as the left one"
    );
    assert!(play.cell(x, y).modifier.contains(Modifier::DIM));

    play.event(newer(None));
    assert!(!play.render().contains("is available"));
}

/// On a terminal too narrow for both, the session's own status keeps the line.
#[test]
fn the_notice_gives_way_to_the_status_text() {
    let mut play = Screenplay::new(40, 14);
    play.event(newer(Some("0.2.0")));
    let grid = play.render();
    assert!(!grid.contains("is available"), "{grid}");
    assert!(grid.contains("efference — ↑ controls"), "{grid}");
}
