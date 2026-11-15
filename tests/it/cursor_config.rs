//! `cursor { … }` in the config.

use jff_wm::config::{Config, CursorConfig, MAX_CURSOR_HIDE_AFTER_MS, MAX_CURSOR_SIZE};

#[test]
fn no_block_changes_nothing() {
    let config = Config::parse("").unwrap();
    assert_eq!(*config.cursor(), CursorConfig::default());
    assert_eq!(Config::default().cursor(), config.cursor());
    assert!(!config.cursor().hide_when_typing);
    assert_eq!(config.cursor().hide_after_ms, 0);
}

#[test]
fn every_setting_is_read() {
    let config = Config::parse(
        r#"cursor {
            theme "Bibata-Modern-Ice"
            size 32
            hide-when-typing
            hide-after-ms 3000
        }"#,
    )
    .unwrap();
    let c = config.cursor();
    assert_eq!(c.theme.as_deref(), Some("Bibata-Modern-Ice"));
    assert_eq!(c.size, Some(32));
    assert!(c.hide_when_typing);
    assert_eq!(c.hide_after_ms, 3000);
}

#[test]
fn hide_when_typing_can_be_turned_off_again() {
    let config = Config::parse("cursor { hide-when-typing #false; }").unwrap();
    assert!(!config.cursor().hide_when_typing);
}

#[test]
fn limits_are_enforced() {
    assert!(Config::parse(&format!("cursor {{ size {MAX_CURSOR_SIZE}; }}")).is_ok());
    assert!(Config::parse(&format!("cursor {{ size {}; }}", MAX_CURSOR_SIZE + 1)).is_err());
    assert!(Config::parse("cursor { size 0; }").is_err());
    assert!(Config::parse("cursor { size -4; }").is_err());
    assert!(Config::parse(&format!(
        "cursor {{ hide-after-ms {MAX_CURSOR_HIDE_AFTER_MS}; }}"
    ))
    .is_ok());
    assert!(Config::parse(&format!(
        "cursor {{ hide-after-ms {}; }}",
        MAX_CURSOR_HIDE_AFTER_MS + 1
    ))
    .is_err());
    assert!(Config::parse("cursor { hide-after-ms 0; }").is_ok());
}

#[test]
fn mistakes_are_errors() {
    for bad in [
        "cursor { theme; }",
        "cursor { theme \"\"; }",
        "cursor { theme 3; }",
        "cursor { size \"big\"; }",
        "cursor { hide-when-typing \"sometimes\"; }",
        "cursor { shape \"arrow\"; }",
    ] {
        assert!(Config::parse(bad).is_err(), "accepted {bad:?}");
    }
}

#[test]
fn config_wins_over_the_environment_which_wins_over_the_defaults() {
    let none = CursorConfig::default();
    assert_eq!(none.resolved(None, None), ("default".to_string(), 24));
    assert_eq!(
        none.resolved(Some("Adwaita"), Some(36)),
        ("Adwaita".to_string(), 36)
    );
    let set = CursorConfig {
        theme: Some("Bibata".into()),
        size: Some(48),
        ..CursorConfig::default()
    };
    assert_eq!(
        set.resolved(Some("Adwaita"), Some(36)),
        ("Bibata".to_string(), 48)
    );
    let only_size = CursorConfig {
        size: Some(48),
        ..CursorConfig::default()
    };
    assert_eq!(
        only_size.resolved(Some("Adwaita"), None),
        ("Adwaita".to_string(), 48)
    );
}

#[test]
fn cursor_is_a_known_top_level_node() {
    assert!(jff_wm::config::TOP_LEVEL_NODES.contains(&"cursor"));
    let warnings = Config::check_text("cursor { size 32; }").unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
}
