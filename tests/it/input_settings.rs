//! `input { … }`: pointer device settings in the config.

use jff_wm::config::{
    AccelProfile, ClickMethod, Config, DeviceKind, DeviceSettings, ScrollMethod, TapButtonMap,
};
use jff_wm::primitives::input_settings::button_code;

const TOUCHPAD: &str = "SYNA8004:00 06CB:CD8B Touchpad";
const MOUSE: &str = "Logitech G502";

#[test]
fn no_input_block_keeps_what_jff_always_did() {
    let config = Config::parse("").unwrap();
    let touchpad = config.input().settings_for(TOUCHPAD, DeviceKind::Touchpad);
    assert_eq!(
        touchpad,
        DeviceSettings {
            tap: Some(true),
            ..DeviceSettings::default()
        }
    );
    let mouse = config.input().settings_for(MOUSE, DeviceKind::Mouse);
    assert_eq!(mouse, DeviceSettings::default());
    assert_eq!(Config::default().input(), config.input());
}

#[test]
fn a_touchpad_block_sets_everything_it_names() {
    let config = Config::parse(
        r#"input {
            touchpad {
                tap #false
                tap-button-map "left-middle-right"
                drag
                natural-scroll
                accel-speed 0.4
                accel-profile "flat"
                scroll-method "two-finger"
                disable-while-typing
                click-method "clickfinger"
                middle-emulation
                left-handed #false
                disable-on-external-mouse
            }
        }"#,
    )
    .unwrap();
    let s = config.input().settings_for(TOUCHPAD, DeviceKind::Touchpad);
    assert_eq!(s.tap, Some(false));
    assert_eq!(s.tap_button_map, Some(TapButtonMap::LeftMiddleRight));
    assert_eq!(s.drag, Some(true));
    assert_eq!(s.natural_scroll, Some(true));
    assert_eq!(s.accel_speed, Some(0.4));
    assert_eq!(s.accel_profile, Some(AccelProfile::Flat));
    assert_eq!(s.scroll_method, Some(ScrollMethod::TwoFinger));
    assert_eq!(s.disable_while_typing, Some(true));
    assert_eq!(s.click_method, Some(ClickMethod::Clickfinger));
    assert_eq!(s.middle_emulation, Some(true));
    assert_eq!(s.left_handed, Some(false));
    assert_eq!(s.disable_on_external_mouse, Some(true));
    assert_eq!(s.off, None);
}

#[test]
fn touchpad_and_mouse_blocks_stay_apart() {
    let config =
        Config::parse("input { touchpad { natural-scroll; }; mouse { accel-profile \"flat\"; } }")
            .unwrap();
    let touchpad = config.input().settings_for(TOUCHPAD, DeviceKind::Touchpad);
    let mouse = config.input().settings_for(MOUSE, DeviceKind::Mouse);
    assert_eq!(touchpad.natural_scroll, Some(true));
    assert_eq!(touchpad.accel_profile, None);
    assert_eq!(mouse.natural_scroll, None);
    assert_eq!(mouse.accel_profile, Some(AccelProfile::Flat));
}

#[test]
fn a_device_block_wins_over_its_kind_and_only_for_that_name() {
    let config = Config::parse(
        r#"input {
            mouse { accel-speed -0.2; left-handed; }
            device "Logitech G502" { accel-speed 0.8; }
        }"#,
    )
    .unwrap();
    let g502 = config.input().settings_for(MOUSE, DeviceKind::Mouse);
    assert_eq!(g502.accel_speed, Some(0.8));
    assert_eq!(g502.left_handed, Some(true));
    let other = config
        .input()
        .settings_for("Other Mouse", DeviceKind::Mouse);
    assert_eq!(other.accel_speed, Some(-0.2));
}

#[test]
fn later_device_blocks_for_a_name_win() {
    let config = Config::parse(
        r#"input {
            device "M" { natural-scroll; left-handed; }
            device "M" { natural-scroll #false; }
        }"#,
    )
    .unwrap();
    let s = config.input().settings_for("M", DeviceKind::Mouse);
    assert_eq!(s.natural_scroll, Some(false));
    assert_eq!(s.left_handed, Some(true));
}

#[test]
fn a_touchpad_block_can_turn_tap_off() {
    let config = Config::parse("input { touchpad { tap #false; } }").unwrap();
    let s = config.input().settings_for(TOUCHPAD, DeviceKind::Touchpad);
    assert_eq!(s.tap, Some(false));
}

#[test]
fn scroll_button_takes_a_name_or_a_code() {
    let name = Config::parse("input { mouse { scroll-button \"side\"; } }").unwrap();
    let code = Config::parse("input { mouse { scroll-button 274; } }").unwrap();
    let side = button_code("side").unwrap();
    assert_eq!(side, 0x113);
    assert_eq!(name.input().mouse.scroll_button, Some(side),);
    assert_eq!(code.input().mouse.scroll_button, Some(274));
    assert!(Config::parse("input { mouse { scroll-button \"nope\"; } }").is_err());
    assert!(Config::parse("input { mouse { scroll-button 0; } }").is_err());
    assert!(Config::parse("input { mouse { scroll-button -3; } }").is_err());
}

#[test]
fn off_is_its_own_setting() {
    let config = Config::parse("input { device \"Touchpad\" { off; } }").unwrap();
    let s = config
        .input()
        .settings_for("Touchpad", DeviceKind::Touchpad);
    assert_eq!(s.off, Some(true));
}

#[test]
fn mistakes_in_the_block_are_errors() {
    for bad in [
        "input { keyboard { } }",
        "input { touchpad { taps; } }",
        "input { touchpad { accel-speed 1.5; } }",
        "input { touchpad { accel-speed -1.01; } }",
        "input { touchpad { accel-speed \"fast\"; } }",
        "input { touchpad { accel-profile \"sharp\"; } }",
        "input { touchpad { scroll-method \"three-finger\"; } }",
        "input { touchpad { click-method \"tap\"; } }",
        "input { touchpad { tap-button-map \"lmr\"; } }",
        "input { touchpad { tap \"maybe\"; } }",
        "input { device { natural-scroll; } }",
    ] {
        assert!(Config::parse(bad).is_err(), "accepted {bad:?}");
    }
}

#[test]
fn accel_speed_takes_the_whole_range_and_whole_numbers() {
    for (text, want) in [("-1", -1.0), ("1", 1.0), ("0", 0.0), ("-0.5", -0.5)] {
        let config =
            Config::parse(&format!("input {{ mouse {{ accel-speed {text}; }} }}")).unwrap();
        assert_eq!(config.input().mouse.accel_speed, Some(want), "{text}");
    }
}

#[test]
fn input_is_a_known_top_level_node() {
    assert!(jff_wm::config::TOP_LEVEL_NODES.contains(&"input"));
    let warnings = Config::check_text("input { touchpad { tap; } }").unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn builtin_then_override_merges_field_by_field() {
    let base = DeviceSettings::builtin(DeviceKind::Touchpad);
    let over = DeviceSettings {
        natural_scroll: Some(true),
        ..DeviceSettings::default()
    };
    let merged = base.overridden_by(&over);
    assert_eq!(merged.tap, Some(true));
    assert_eq!(merged.natural_scroll, Some(true));
    assert_eq!(DeviceSettings::builtin(DeviceKind::Mouse).tap, None);
}
