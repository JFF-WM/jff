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
