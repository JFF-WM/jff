//! The config nodes that decide how client-bound global hotkeys and `forward-key` fit together.

use jff_wm::config::{Action, Config};
use jff_wm::protocol::xx_hotkey::{mods_state, CTRL, SHIFT};
use smithay::input::keyboard::Keysym;

#[test]
fn global_hotkeys_are_off_unless_asked_for() {
    assert!(!Config::parse("").unwrap().global_hotkeys());
    assert!(Config::parse("global-hotkeys #true")
        .unwrap()
        .global_hotkeys());
    assert!(!Config::parse("global-hotkeys #false")
        .unwrap()
        .global_hotkeys());
}

#[test]
fn overview_animation_has_a_default_and_a_range() {
    assert_eq!(Config::parse("").unwrap().overview().animation_ms, 320);
    let config = Config::parse("overview { animation-ms 500; }").unwrap();
    assert_eq!(config.overview().animation_ms, 500);
    let config = Config::parse("overview { animation-ms 0; }").unwrap();
    assert_eq!(config.overview().animation_ms, 0);
    assert!(Config::parse("overview { animation-ms 2001; }").is_err());
    assert!(Config::parse("overview { animation-ms -1; }").is_err());
}

#[test]
fn forward_key_binds_a_key_to_an_app_id() {
    let config = Config::parse(
        r#"
binds {
    Ctrl+Shift+g { forward-key "com.obsproject.Studio"; }
}
"#,
    )
    .unwrap();
    let action = config.action_for(&mods_state(CTRL | SHIFT), Keysym::g);
    assert!(
        matches!(action, Some(Action::ForwardKey(app_id)) if app_id == "com.obsproject.Studio"),
        "{action:?}"
    );
}

#[test]
fn forward_key_needs_exactly_one_app_id() {
    for line in ["forward-key", "forward-key \"\"", "forward-key \"a\" \"b\""] {
        let text = format!("binds {{ Ctrl+Shift+g {{ {line}; }} }}");
        assert!(Config::parse(&text).is_err(), "{line}");
    }
}

#[test]
fn layer_radius_is_square_unless_asked_for() {
    let config = Config::parse("blur { passes 3; }").unwrap();
    assert_eq!(config.blur_layers().radius_for("JFShell-Bottom"), 0);
    assert_eq!(config.blur_layers().radius_for("anything"), 0);
}

#[test]
fn layer_radius_applies_to_every_namespace() {
    let config = Config::parse("blur { layer-radius 16; }").unwrap();
    assert_eq!(config.blur_layers().radius_for("JFShell-Bottom"), 16);
    assert_eq!(config.blur_layers().radius_for("JFShell-Top"), 16);
}

#[test]
fn layer_radius_per_namespace_wins_over_the_general_one() {
    let config = Config::parse(
        "blur { layer-radius 16; layer-radius \"JFShell-Bottom\" 24; layer-radius \"selection\" 0; }",
    )
    .unwrap();
    let layers = config.blur_layers();
    assert_eq!(layers.radius_for("JFShell-Bottom"), 24);
    assert_eq!(layers.radius_for("selection"), 0);
    assert_eq!(layers.radius_for("JFShell-Top"), 16);
}

#[test]
fn layer_radius_works_without_a_general_value() {
    let config = Config::parse("blur { layer-radius \"JFShell-Bottom\" 12; }").unwrap();
    assert_eq!(config.blur_layers().radius_for("JFShell-Bottom"), 12);
    assert_eq!(config.blur_layers().radius_for("JFShell-Top"), 0);
}

#[test]
fn layer_radius_has_a_range_and_a_shape() {
    for line in [
        "layer-radius",
        "layer-radius -1",
        "layer-radius 257",
        "layer-radius \"JFShell-Bottom\"",
        "layer-radius 16 \"JFShell-Bottom\"",
        "layer-radius \"a\" \"b\" 16",
        "layer-radius 1.5",
    ] {
        let text = format!("blur {{ {line}; }}");
        assert!(Config::parse(&text).is_err(), "{line}");
    }
}

#[test]
fn close_kill_is_off_by_default_and_waits_five_seconds() {
    use std::time::Duration;
    for text in ["", "close-kill { }", "close-kill { seconds 9; }"] {
        let config = Config::parse(text).unwrap();
        assert!(!config.close_kill().enable, "{text:?}");
    }
    let config = Config::parse("").unwrap();
    assert_eq!(config.close_kill().timeout, Duration::from_secs(5));
}

#[test]
fn close_kill_takes_enable_and_seconds() {
    use std::time::Duration;
    let config = Config::parse("close-kill { enable; }").unwrap();
    assert!(config.close_kill().enable);
    assert_eq!(config.close_kill().timeout, Duration::from_secs(5));

    let config = Config::parse("close-kill { enable; seconds 12; }").unwrap();
    assert!(config.close_kill().enable);
    assert_eq!(config.close_kill().timeout, Duration::from_secs(12));

    let config = Config::parse("close-kill { enable; seconds 2.5; }").unwrap();
    assert_eq!(config.close_kill().timeout, Duration::from_millis(2500));

    let config = Config::parse("close-kill { enable #false; seconds 3; }").unwrap();
    assert!(!config.close_kill().enable);
}

#[test]
fn close_kill_seconds_has_a_range_and_unknown_keys_are_errors() {
    for line in [
        "seconds 0",
        "seconds 0.5",
        "seconds -1",
        "seconds 3601",
        "seconds",
        "seconds \"x\"",
    ] {
        let text = format!("close-kill {{ enable; {line}; }}");
        assert!(Config::parse(&text).is_err(), "{line}");
    }
    assert!(Config::parse("close-kill { enable; timeout 5; }").is_err());
}

#[test]
fn check_config_warns_about_close_kill_settings_without_enable() {
    let warnings = Config::check_text("close-kill { seconds 3; }").unwrap();
    assert!(
        warnings.iter().any(|w| w.contains("close-kill")),
        "{warnings:?}"
    );
    let warnings = Config::check_text("close-kill { enable; seconds 3; }").unwrap();
    assert!(
        !warnings.iter().any(|w| w.contains("close-kill")),
        "{warnings:?}"
    );
}

#[test]
fn close_kill_seconds_read_as_text() {
    use jff_wm::feature::close_watch::seconds_text;
    use std::time::Duration;
    assert_eq!(seconds_text(Duration::from_secs(1)), "1 second");
    assert_eq!(seconds_text(Duration::from_secs(5)), "5 seconds");
    assert_eq!(seconds_text(Duration::from_millis(2500)), "2.5 seconds");
    assert_eq!(seconds_text(Duration::from_millis(1250)), "1.25 seconds");
}
