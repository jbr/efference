use super::*;
use std::path::PathBuf;

fn home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("eff-tui-config-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".efference")).unwrap();
    home
}

#[test]
fn the_table_parses_beside_the_connections_and_expands_home() {
    let home = home("parse");
    std::fs::write(
        home.join(".efference/config.toml"),
        r#"
[tui]
theme = "~/themes/mine.tmTheme"
images = "iterm2"
frame-cost = true

[connections.work]
provider = "anthropic"
auth = { kind = "env", key = "K" }
"#,
    )
    .unwrap();
    let tui = TuiConfig::load(&home).unwrap();
    assert_eq!(
        tui,
        TuiConfig {
            theme: Some(home.join("themes/mine.tmTheme").display().to_string()),
            images: ImageMode::Iterm2,
            frame_cost: true,
        }
    );
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn a_missing_table_is_the_defaults_and_an_unknown_key_or_mode_is_refused() {
    let home = home("edges");
    assert_eq!(
        TuiConfig::load(&home.join("nowhere")).unwrap(),
        TuiConfig::default()
    );
    std::fs::write(
        home.join(".efference/config.toml"),
        "[tui]\ntheme = \"Nord\"\n",
    )
    .unwrap();
    let tui = TuiConfig::load(&home).unwrap();
    assert_eq!(
        (tui.theme.as_deref(), tui.images, tui.frame_cost),
        (Some("Nord"), ImageMode::Auto, false)
    );
    for refused in ["[tui]\nimages = \"sixel\"\n", "[tui]\ncolour = \"x\"\n"] {
        std::fs::write(home.join(".efference/config.toml"), refused).unwrap();
        assert!(TuiConfig::load(&home).is_err(), "{refused}");
    }
    std::fs::remove_dir_all(&home).unwrap();
}
