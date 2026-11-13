//! Tests for `jff_wm::config`.

use std::time::Duration;
use std::time::SystemTime;

use smithay::input::keyboard::xkb;
use smithay::input::keyboard::Keysym;
use smithay::input::keyboard::ModifiersState;

use jff_wm::config::{
    closest, edit_distance, parse_color, Action, BorderAnimation, Config, ConfigWatch, FileStamp,
    HotCorner, KeyboardConfig, LogLevel, ModeRequest, ScreenshotConfig, Vrr,
    DEFAULT_BORDER_FADE_MS, DEFAULT_BORDER_PERIOD_MS, DEFAULT_BORDER_WIDTH, DEFAULT_CONFIG,
    DEFAULT_LOG_PATH, DEFAULT_REPEAT_DELAY, DEFAULT_REPEAT_RATE, DEFAULT_SELECTION_COLOR,
    DEFAULT_SELECTION_WIDTH, DEFAULT_SWITCHER_SLIDE_MS, DEFAULT_WORKSPACES, MAX_BORDER_PERIOD_MS,
    MAX_BORDER_WIDTH, MAX_GAPS, MAX_RADIUS, TOP_LEVEL_NODES,
};
use jff_wm::config::{RuleOutput, WindowFacts, WindowMatch};
use jff_wm::desk::{Goto, Side};
use jff_wm::primitives::hdr_output::ToneMap;

fn mods(ctrl: bool, alt: bool, shift: bool, logo: bool) -> ModifiersState {
    ModifiersState {
        ctrl,
        alt,
        shift,
        logo,
        ..Default::default()
    }
}

#[test]
fn new_windows_maximize_unless_disabled() {
    assert!(Config::default().maximize_new_windows());
    assert!(!Config::parse("maximize-new-windows #false")
        .unwrap()
        .maximize_new_windows());
    assert!(Config::parse("maximize-new-windows #true")
        .unwrap()
        .maximize_new_windows());
    assert!(Config::parse("maximize-new-windows 3").is_err());
}

#[test]
fn csd_is_kept_unless_no_csd_is_preferred() {
    assert!(!Config::default().no_client_decorations());
    assert!(Config::parse("no-client-decorations")
        .unwrap()
        .no_client_decorations());
    assert!(Config::parse("no-client-decorations #true")
        .unwrap()
        .no_client_decorations());
    assert!(!Config::parse("no-client-decorations #false")
        .unwrap()
        .no_client_decorations());
    assert!(Config::parse("no-client-decorations 3").is_err());
}

#[test]
fn drag_modifier_is_alt_unless_set() {
    let alt = mods(false, true, false, false);
    let logo = mods(false, false, false, true);
    assert!(Config::default().drag_modifier_held(&alt));
    assert!(!Config::default().drag_modifier_held(&logo));
    let config = Config::parse("drag-modifier \"Super\"").unwrap();
    assert!(config.drag_modifier_held(&logo));
    assert!(!config.drag_modifier_held(&alt));
    assert!(Config::parse("drag-modifier \"Hyper\"").is_err());
}

#[test]
fn keyboard_defaults_match_builtin_behaviour() {
    let keyboard = Config::default().keyboard().clone();
    assert_eq!(keyboard, KeyboardConfig::default());
    assert_eq!(keyboard.repeat_delay, DEFAULT_REPEAT_DELAY);
    assert_eq!(keyboard.repeat_rate, DEFAULT_REPEAT_RATE);
    assert!(Config::parse("").unwrap().keyboard().layout.is_empty());
    assert!(keyboard.numlock);
}

#[test]
fn numlock_can_be_disabled() {
    assert!(
        Config::parse("keyboard { numlock #true; }")
            .unwrap()
            .keyboard()
            .numlock
    );
    assert!(
        !Config::parse("keyboard { numlock #false; }")
            .unwrap()
            .keyboard()
            .numlock
    );
    assert!(
        Config::parse("keyboard { numlock; }")
            .unwrap()
            .keyboard()
            .numlock
    );
    assert!(Config::parse(r#"keyboard { numlock "maybe"; }"#).is_err());
}

#[test]
fn keyboard_is_parsed() {
    let config = Config::parse(
        r#"keyboard {
                xkb { layout "us"; variant "intl"; options "caps:escape"; }
                repeat-delay 300
                repeat-rate 40
            }"#,
    )
    .unwrap();
    let keyboard = config.keyboard();
    assert_eq!(keyboard.layout, "us");
    assert_eq!(keyboard.variant, "intl");
    assert_eq!(keyboard.options.as_deref(), Some("caps:escape"));
    assert_eq!((keyboard.repeat_delay, keyboard.repeat_rate), (300, 40));
    assert!(keyboard.keymap_differs(&KeyboardConfig::default()));
}

#[test]
fn rejects_bad_keyboard() {
    assert!(Config::parse("keyboard { repeat-delay -1; }").is_err());
    assert!(Config::parse("keyboard { repeat-delay 20000; }").is_err());
    assert!(Config::parse("keyboard { repeat-rate; }").is_err());
    assert!(Config::parse(r#"keyboard { repeat-rate "fast"; }"#).is_err());
    assert!(Config::parse("keyboard { speed 3; }").is_err());
    assert!(Config::parse("keyboard { xkb { colour \"x\"; } }").is_err());
    assert!(Config::parse("keyboard { xkb { layout 3; } }").is_err());
    assert!(Config::parse(r#"keyboard { xkb { layout "nosuchlayout"; } }"#).is_err());
}

#[test]
fn default_config_parses() {
    let config = Config::default();
    assert_eq!(
        config.action_for(&mods(false, true, false, false), Keysym::m),
        Some(&Action::Maximize)
    );
}

#[test]
fn screenshot_actions_are_parsed_from_their_names() {
    let config = Config::parse(
        r#"binds {
                Print { screenshot-area; }
                Ctrl+Print { screenshot-fullscreen; }
                Alt+Print { screenshot-window; }
            }"#,
    )
    .unwrap();
    assert_eq!(
        config.action_for(&mods(false, false, false, false), Keysym::Print),
        Some(&Action::ScreenshotArea)
    );
    assert_eq!(
        config.action_for(&mods(true, false, false, false), Keysym::Print),
        Some(&Action::ScreenshotFullscreen)
    );
    assert_eq!(
        config.action_for(&mods(false, true, false, false), Keysym::Print),
        Some(&Action::ScreenshotWindow)
    );
}

#[test]
fn screenshot_actions_take_no_arguments_and_have_no_other_spelling() {
    assert!(Config::parse(r#"binds { Print { screenshot-area "x"; } }"#).is_err());
    assert!(Config::parse("binds { Print { screenshot-fullscreen 1; } }").is_err());
    assert!(Config::parse("binds { Print { screenshot_window; } }").is_err());
    assert!(Config::parse("binds { Print { screenshot; } }").is_err());
}

#[test]
fn toggle_shortcuts_inhibit_is_bound_by_default_and_takes_no_arguments() {
    assert!(Config::parse("binds { Super+Escape { toggle-shortcuts-inhibit; } }").is_ok());
    assert!(Config::parse("binds { Super+Escape { toggle-shortcuts-inhibit 1; } }").is_err());
    assert!(DEFAULT_CONFIG.contains("toggle-shortcuts-inhibit"));
}

#[test]
fn screenshots_are_bound_by_default() {
    let config = Config::default();
    assert_eq!(
        config.action_for(&mods(false, false, false, false), Keysym::Print),
        Some(&Action::ScreenshotArea)
    );
    assert_eq!(
        config.action_for(&mods(true, false, false, false), Keysym::Print),
        Some(&Action::ScreenshotFullscreen)
    );
    assert_eq!(
        config.action_for(&mods(false, true, false, false), Keysym::Print),
        Some(&Action::ScreenshotWindow)
    );
}

#[test]
fn screenshot_block_defaults_and_settings() {
    let config = Config::default();
    assert_eq!(config.screenshot(), &ScreenshotConfig::default());
    assert!(!config.screenshot().include_cursor);
    assert!(config.screenshot().copy_to_clipboard);
    assert_eq!(config.screenshot().path, None);

    let config = Config::parse(
        r#"screenshot {
                path "~/Pictures/Shots"
                include-cursor #true
                copy-to-clipboard #false
            }"#,
    )
    .unwrap();
    assert_eq!(
        config.screenshot().path.as_deref(),
        Some("~/Pictures/Shots")
    );
    assert!(config.screenshot().include_cursor);
    assert!(!config.screenshot().copy_to_clipboard);

    // A bare flag means on, an empty block changes nothing.
    let config = Config::parse("screenshot { include-cursor; }").unwrap();
    assert!(config.screenshot().include_cursor);
    assert_eq!(
        Config::parse("screenshot {}").unwrap().screenshot(),
        &ScreenshotConfig::default()
    );
}

#[test]
fn rejects_bad_screenshot_block() {
    assert!(Config::parse("screenshot { path 3; }").is_err());
    assert!(Config::parse("screenshot { path; }").is_err());
    assert!(Config::parse("screenshot { include-cursor 3; }").is_err());
    assert!(Config::parse("screenshot { copy-to-clipboard maybe; }").is_err());
    assert!(Config::parse("screenshot { format \"jpg\"; }").is_err());
}

#[test]
fn overview_is_bound_by_default() {
    let config = Config::default();
    assert_eq!(
        config.action_for(&mods(false, true, false, false), Keysym::o),
        Some(&Action::Overview)
    );
    assert!(Config::parse("binds { Alt+O { overview 1; } }").is_err());
}

#[test]
fn window_switcher_is_bound_by_default() {
    let config = Config::default();
    assert_eq!(
        config.action_for(&mods(false, true, false, false), Keysym::Tab),
        Some(&Action::SwitchWindow { backwards: false })
    );
    assert_eq!(
        config.action_for(&mods(false, true, true, false), Keysym::Tab),
        Some(&Action::SwitchWindow { backwards: true })
    );
    assert!(Config::parse("binds { Alt+Tab { switch-window 1; } }").is_err());
}

fn key(name: &str) -> Keysym {
    xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE)
}

#[test]
fn workspaces_are_bound_by_default() {
    let config = Config::default();
    let alt = mods(false, true, false, false);
    let alt_shift = mods(false, true, true, false);
    assert_eq!(config.workspaces(), DEFAULT_WORKSPACES);
    assert_eq!(
        config.action_for(&alt, key("3")),
        Some(&Action::Workspace(Goto::To(2)))
    );
    assert_eq!(
        config.action_for(&alt_shift, key("9")),
        Some(&Action::MoveToWorkspace(8))
    );
    assert_eq!(
        config.action_for(&mods(true, true, false, false), key("Right")),
        Some(&Action::Workspace(Goto::Next))
    );
}

#[test]
fn workspace_actions_and_count_are_parsed() {
    let config = Config::parse(
        "workspaces 3\nbinds { Super+1 { workspace 1; }; Super+P { workspace-prev; } }",
    )
    .unwrap();
    let logo = mods(false, false, false, true);
    assert_eq!(config.workspaces(), 3);
    assert_eq!(
        config.action_for(&logo, key("1")),
        Some(&Action::Workspace(Goto::To(0)))
    );
    assert_eq!(
        config.action_for(&logo, key("p")),
        Some(&Action::Workspace(Goto::Prev))
    );
}

#[test]
fn rejects_bad_workspaces() {
    assert!(Config::parse("workspaces 0").is_err());
    assert!(Config::parse("workspaces 65").is_err());
    assert!(Config::parse("workspaces").is_err());
    assert!(Config::parse(r#"workspaces "four""#).is_err());
    assert!(Config::parse("binds { Alt+1 { workspace; } }").is_err());
    assert!(Config::parse("binds { Alt+1 { workspace 0; } }").is_err());
    assert!(Config::parse(r#"binds { Alt+1 { workspace "1"; } }"#).is_err());
    assert!(Config::parse("binds { Alt+1 { move-to-workspace 1 2; } }").is_err());
    assert!(Config::parse("binds { Alt+1 { workspace-next 1; } }").is_err());
}

#[test]
fn mouse_buttons_can_be_bound() {
    let config =
        Config::parse("binds { MouseBack { overview; }; Alt+MouseForward { maximize; } }").unwrap();
    let none = mods(false, false, false, false);
    let alt = mods(false, true, false, false);
    assert_eq!(
        config.button_action_for(&none, 0x113),
        Some(&Action::Overview)
    );
    assert_eq!(
        config.button_action_for(&none, 0x116),
        Some(&Action::Overview)
    );
    assert_eq!(config.button_action_for(&none, 0x114), None);
    assert_eq!(
        config.button_action_for(&alt, 0x115),
        Some(&Action::Maximize)
    );
    // A button bind is not a key bind, and the left button is not bindable.
    assert_eq!(config.action_for(&none, Keysym::o), None);
    assert_eq!(config.button_action_for(&none, 0x110), None);
}

#[test]
fn autostart_is_parsed_in_order() {
    let config = Config::parse(
        r#"autostart "waybar"
autostart "swaybg" "-i" "/tmp/w.png"
sh-autostart "a && b""#,
    )
    .unwrap();
    assert_eq!(
        config.autostart(),
        &[
            Action::Spawn(vec!["waybar".into()]),
            Action::Spawn(vec!["swaybg".into(), "-i".into(), "/tmp/w.png".into()]),
            Action::SpawnShell("a && b".into()),
        ]
    );
    assert!(Config::default().autostart().is_empty());
}

#[test]
fn bad_autostart_is_an_error() {
    assert!(Config::parse("autostart").is_err());
    assert!(Config::parse("autostart 5").is_err());
    assert!(Config::parse("sh-autostart").is_err());
    assert!(Config::parse("sh-autostart \"a\" \"b\"").is_err());
}

#[test]
fn spawn_keeps_arguments() {
    let config = Config::parse(r#"binds { Super+T { spawn "foot" "-e" "htop"; } }"#).unwrap();
    assert_eq!(
        config.action_for(&mods(false, false, false, true), Keysym::t),
        Some(&Action::Spawn(vec![
            "foot".into(),
            "-e".into(),
            "htop".into()
        ]))
    );
}

#[test]
fn modifiers_match_exactly() {
    let config = Config::parse("binds { Alt+M { maximize; } }").unwrap();
    assert!(config
        .action_for(&mods(false, true, true, false), Keysym::m)
        .is_none());
}

#[test]
fn outputs_are_parsed() {
    let config = Config::parse(
        r#"outputs {
                output "DP-1" { position 1920 0; mode "2560x1440@143.97"; }
                output "eDP-1" { off; }
            }"#,
    )
    .unwrap();
    let dp = config.output("DP-1").unwrap();
    assert_eq!(dp.position, Some((1920, 0)));
    assert_eq!(
        dp.mode,
        Some(ModeRequest {
            width: 2560,
            height: 1440,
            refresh: Some(144)
        })
    );
    assert!(config.output("eDP-1").unwrap().off);
    assert!(config.output("HDMI-A-1").is_none());
}

#[test]
fn update_output_edits_or_adds() {
    let mut config = Config::parse(r#"outputs { output "DP-1" { position 5 5; } }"#).unwrap();
    config.update_output("DP-1", |c| c.off = true);
    config.update_output("HDMI-A-1", |c| c.position = Some((1, 2)));
    assert!(config.output("DP-1").unwrap().off);
    assert_eq!(config.output("DP-1").unwrap().position, Some((5, 5)));
    assert_eq!(config.output("HDMI-A-1").unwrap().position, Some((1, 2)));
}

#[test]
fn rejects_bad_outputs() {
    assert!(Config::parse(r#"outputs { monitor "DP-1" { off; } }"#).is_err());
    assert!(Config::parse(r#"outputs { output "DP-1" { position 1; } }"#).is_err());
    let config =
        Config::parse(r#"outputs { output "DP-1" { icc-profile "/tmp/a.icc"; } }"#).unwrap();
    assert_eq!(
        config.output("DP-1").unwrap().icc_profile.as_deref(),
        Some(std::path::Path::new("/tmp/a.icc"))
    );
    assert!(Config::parse(r#"outputs { output "DP-1" { icc-profile; } }"#).is_err());
    let config = Config::parse(r#"outputs { output "DP-1" { adaptive-sync "always"; } }"#).unwrap();
    assert_eq!(config.output("DP-1").unwrap().vrr, Vrr::Always);
    let config = Config::parse(r#"outputs { output "DP-1" { content-type-hint; } }"#).unwrap();
    assert!(config.output("DP-1").unwrap().content_type_hint);
    assert!(!config.output("DP-2").map_or(false, |c| c.content_type_hint));
    assert!(Config::parse(r#"outputs { output "DP-1" { content-type-hint true; } }"#).is_err());
    assert!(Config::parse(r#"outputs { output "DP-1" { adaptive-sync "maybe"; } }"#).is_err());
    assert!(Config::parse(r#"outputs { output "DP-1" { mode "big"; } }"#).is_err());
    assert!(Config::parse(r#"outputs { output { off; } }"#).is_err());
}

#[test]
fn rejects_bad_input() {
    assert!(Config::parse("binds { Hyper+M { maximize; } }").is_err());
    assert!(Config::parse("binds { Alt+NoSuchKey { maximize; } }").is_err());
    assert!(Config::parse("binds { Alt+M { fly; } }").is_err());
    assert!(Config::parse("binds { Alt+M { spawn; } }").is_err());
    assert!(Config::parse("binds { Alt+M { maximize 1; } }").is_err());
}

#[test]
fn gaps_default_to_zero() {
    assert_eq!(Config::default().gaps(), 0);
    assert_eq!(Config::parse("").unwrap().gaps(), 0);
    assert_eq!(Config::parse("workspaces 3").unwrap().gaps(), 0);
}

#[test]
fn gaps_are_read() {
    assert_eq!(Config::parse("gaps 10").unwrap().gaps(), 10);
    assert_eq!(Config::parse("gaps 0").unwrap().gaps(), 0);
    assert_eq!(
        Config::parse(&format!("gaps {MAX_GAPS}")).unwrap().gaps(),
        MAX_GAPS
    );
}

#[test]
fn gaps_reject_bad_values() {
    assert!(Config::parse("gaps").is_err());
    assert!(Config::parse("gaps -1").is_err());
    assert!(Config::parse(&format!("gaps {}", MAX_GAPS + 1)).is_err());
    assert!(Config::parse("gaps 1.5").is_err());
    assert!(Config::parse("gaps \"big\"").is_err());
    assert!(Config::parse("gaps 4 6").is_err());
}

#[test]
fn radius_defaults_to_square_corners() {
    assert_eq!(Config::default().radius(), 0);
    assert_eq!(Config::parse("").unwrap().radius(), 0);
    assert_eq!(Config::parse("gaps 10").unwrap().radius(), 0);
}

#[test]
fn radius_is_read() {
    assert_eq!(Config::parse("radius 12").unwrap().radius(), 12);
    assert_eq!(Config::parse("radius 0").unwrap().radius(), 0);
    assert_eq!(
        Config::parse(&format!("radius {MAX_RADIUS}"))
            .unwrap()
            .radius(),
        MAX_RADIUS
    );
    // Independent of gaps.
    let both = Config::parse("gaps 8\nradius 12").unwrap();
    assert_eq!((both.gaps(), both.radius()), (8, 12));
}

#[test]
fn popup_radius_follows_radius_unless_set() {
    assert_eq!(Config::default().popup_radius(), 0);
    assert_eq!(Config::parse("radius 12").unwrap().popup_radius(), 12);
    let own = Config::parse("radius 16\npopup-radius 10").unwrap();
    assert_eq!((own.radius(), own.popup_radius()), (16, 10));
    assert_eq!(
        Config::parse("popup-radius 0\nradius 8")
            .unwrap()
            .popup_radius(),
        0
    );
    assert!(Config::parse("popup-radius -1").is_err());
    assert!(Config::parse(&format!("popup-radius {}", MAX_RADIUS + 1)).is_err());
}

#[test]
fn colours_parse() {
    assert_eq!(parse_color("#000").unwrap(), [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(parse_color("#fff").unwrap(), [1.0, 1.0, 1.0, 1.0]);
    assert_eq!(parse_color("#ff0000").unwrap(), [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(
        parse_color("#0f08").unwrap(),
        [0.0, 1.0, 0.0, 136.0 / 255.0]
    );
    assert_eq!(parse_color("#00000000").unwrap()[3], 0.0);
    assert_eq!(parse_color("#FFFFFF80").unwrap()[3], 128.0 / 255.0);
    assert!(parse_color("ffffff").is_err());
    assert!(parse_color("#ff").is_err());
    assert!(parse_color("#fffff").is_err());
    assert!(parse_color("#gggggg").is_err());
    assert!(parse_color("#").is_err());
}

#[test]
fn border_is_off_without_a_block_and_on_with_one() {
    assert!(!Config::default().border().on());
    assert!(!Config::parse("gaps 4").unwrap().border().on());
    let on = Config::parse("border {}").unwrap().border();
    assert!(on.on());
    assert_eq!(on.width, DEFAULT_BORDER_WIDTH);
    assert_eq!(on.fade_ms, DEFAULT_BORDER_FADE_MS);
    // Nothing moves unless asked to.
    assert_eq!(on.animate, BorderAnimation::None);
    assert_eq!(on.period_ms, DEFAULT_BORDER_PERIOD_MS);
    assert_eq!(on.focus_color_2(), on.focus_color);
    assert!(!on.animated());
}

#[test]
fn border_animation_is_read() {
    let b =
        Config::parse("border { animate \"spin\"; period-ms 2000; focus-color-2 \"#ff0000\"; }")
            .unwrap()
            .border();
    assert_eq!(b.animate, BorderAnimation::Spin);
    assert_eq!(b.period_ms, 2000);
    assert_eq!(b.focus_color_2(), [1.0, 0.0, 0.0, 1.0]);
    assert!(b.animated());
    let sweep = Config::parse("border { animate \"sweep\"; focus-color-2 \"#fff\"; }")
        .unwrap()
        .border();
    assert_eq!(sweep.animate, BorderAnimation::Sweep);
    assert_eq!(sweep.period_ms, DEFAULT_BORDER_PERIOD_MS);
    assert!(sweep.animated());
}

#[test]
fn border_animation_needs_all_of_its_parts() {
    // The default second colour is the first one: nothing to see, nothing to render.
    assert!(!Config::parse("border { animate \"spin\"; }")
        .unwrap()
        .border()
        .animated());
    // A period of 0 or "none" stands still.
    let zero = "border { animate \"spin\"; focus-color-2 \"#f00\"; period-ms 0; }";
    assert!(!Config::parse(zero).unwrap().border().animated());
    let none = "border { animate \"spin\"; focus-color-2 \"#f00\"; period-ms \"none\"; }";
    let none = Config::parse(none).unwrap().border();
    assert_eq!(none.period_ms, 0);
    assert!(!none.animated());
    // `animate none` ignores the rest.
    let off = "border { animate \"none\"; focus-color-2 \"#f00\"; }";
    assert!(!Config::parse(off).unwrap().border().animated());
    // No width, no border, no animation.
    let flat = "border { width 0; animate \"spin\"; focus-color-2 \"#f00\"; }";
    assert!(!Config::parse(flat).unwrap().border().animated());
}

#[test]
fn border_settings_are_read() {
    let b = Config::parse(
        "border { width 3; color \"#102030\"; focus-color \"#ff000080\"; fade-ms 0; }",
    )
    .unwrap()
    .border();
    assert_eq!(b.width, 3);
    assert_eq!(b.color, [16.0 / 255.0, 32.0 / 255.0, 48.0 / 255.0, 1.0]);
    assert_eq!(b.focus_color, [1.0, 0.0, 0.0, 128.0 / 255.0]);
    assert_eq!(b.fade_ms, 0);
    assert!(!Config::parse("border { width 0; }").unwrap().border().on());
}

#[test]
fn border_rejects_bad_values() {
    assert!(Config::parse("border { width -1; }").is_err());
    assert!(Config::parse(&format!("border {{ width {}; }}", MAX_BORDER_WIDTH + 1)).is_err());
    assert!(Config::parse("border { color 3; }").is_err());
    assert!(Config::parse("border { color \"red\"; }").is_err());
    assert!(Config::parse("border { fade-ms 99999; }").is_err());
    assert!(Config::parse("border { glow 1; }").is_err());
    assert!(Config::parse("border { animate \"pulse\"; }").is_err());
    assert!(Config::parse("border { animate 1; }").is_err());
    assert!(Config::parse("border { period-ms -1; }").is_err());
    assert!(Config::parse("border { period-ms \"fast\"; }").is_err());
    let too_long = format!("border {{ period-ms {}; }}", MAX_BORDER_PERIOD_MS + 1);
    assert!(Config::parse(&too_long).is_err());
    assert!(Config::parse("border { focus-color-2 \"blue\"; }").is_err());
}

#[test]
fn maximized_windows_leave_room_for_the_border() {
    assert_eq!(Config::parse("gaps 10").unwrap().maximize_inset(), 10);
    assert_eq!(
        Config::parse("border { width 4; }")
            .unwrap()
            .maximize_inset(),
        4
    );
    let both = Config::parse("gaps 10\nborder { width 4; }").unwrap();
    assert_eq!(both.maximize_inset(), 10);
    let wide = Config::parse("gaps 2\nborder { width 6; }").unwrap();
    assert_eq!(wide.maximize_inset(), 6);
}

#[test]
fn snapped_windows_leave_room_for_both_borders() {
    assert_eq!(Config::parse("gaps 10").unwrap().snap_gap(), 10);
    assert_eq!(Config::parse("border { width 4; }").unwrap().snap_gap(), 8);
    let both = Config::parse("gaps 10\nborder { width 4; }").unwrap();
    assert_eq!(both.snap_gap(), 10);
    let wide = Config::parse("gaps 2\nborder { width 6; }").unwrap();
    assert_eq!(wide.snap_gap(), 12);
}

fn stamp(len: u64) -> Option<FileStamp> {
    Some(FileStamp {
        modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(len)),
        len,
    })
}

#[test]
fn watch_is_quiet_while_the_file_does_not_change() {
    let mut watch = ConfigWatch::new(stamp(1));
    for _ in 0..5 {
        assert!(!watch.poll(stamp(1)));
    }
    let mut missing = ConfigWatch::new(None);
    assert!(!missing.poll(None));
}

#[test]
fn watch_reloads_once_the_file_has_held_still_for_a_poll() {
    let mut watch = ConfigWatch::new(stamp(1));
    assert!(!watch.poll(stamp(2)), "first sight of a change waits");
    assert!(watch.poll(stamp(2)), "unchanged since: save is done");
    // And then it is the new normal.
    assert!(!watch.poll(stamp(2)));
    assert!(!watch.poll(stamp(2)));
}

#[test]
fn watch_waits_while_the_file_is_still_being_written() {
    let mut watch = ConfigWatch::new(stamp(1));
    assert!(!watch.poll(stamp(2)));
    assert!(!watch.poll(stamp(3)), "changed again: wait again");
    assert!(!watch.poll(stamp(4)));
    assert!(watch.poll(stamp(4)));
}

#[test]
fn watch_ignores_a_change_that_goes_back() {
    // An editor that writes a temporary file and removes it again.
    let mut watch = ConfigWatch::new(stamp(1));
    assert!(!watch.poll(None));
    assert!(!watch.poll(stamp(1)));
    assert!(!watch.poll(stamp(1)));
}

#[test]
fn watch_notices_a_file_appearing_and_disappearing() {
    let mut watch = ConfigWatch::new(None);
    assert!(!watch.poll(stamp(7)));
    assert!(watch.poll(stamp(7)));
    // Deleted: the defaults come back.
    assert!(!watch.poll(None));
    assert!(watch.poll(None));
}

#[test]
fn check_accepts_the_built_in_config_without_warnings() {
    assert_eq!(
        Config::check_text(DEFAULT_CONFIG).unwrap(),
        Vec::<String>::new()
    );
    assert!(Config::check_text("").unwrap().is_empty());
}

#[test]
fn check_reports_errors_like_parse() {
    assert!(Config::check_text("border { glow 1; }").is_err());
    assert!(Config::check_text("gaps -1").is_err());
    assert!(Config::check_text("binds {").is_err());
}

#[test]
fn check_warns_about_ignored_top_level_settings() {
    let warnings = Config::check_text("gap 10\nradius 8\nqqqq 1").unwrap();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("\"gap\"") && warnings[0].contains("\"gaps\""));
    assert!(warnings[1].contains("\"qqqq\"") && !warnings[1].contains("did you mean"));
    // Real settings don't get a warning.
    let known = "workspaces 4\ngaps 4\nautostart \"true\"\nsh-autostart \"true\"\nblur {}";
    assert!(Config::check_text(known).unwrap().is_empty());
}

#[test]
fn check_warns_when_an_animation_cannot_show() {
    let nothing = Config::check_text("border { animate \"spin\"; }").unwrap();
    assert_eq!(nothing.len(), 1, "{nothing:?}");
    assert!(nothing[0].contains("focus-color-2"));
    let still = "border { animate \"spin\"; focus-color-2 \"#f00\"; period-ms 0; }";
    assert!(Config::check_text(still).unwrap()[0].contains("period-ms"));
    let flat = "border { width 0; animate \"spin\"; focus-color-2 \"#f00\"; }";
    assert!(Config::check_text(flat).unwrap()[0].contains("width 0"));
    let works = "border { animate \"spin\"; focus-color-2 \"#f00\"; }";
    assert!(Config::check_text(works).unwrap().is_empty());
    assert!(Config::check_text("border { width 2; }")
        .unwrap()
        .is_empty());
}

#[test]
fn typos_find_their_setting() {
    assert_eq!(closest("gap", TOP_LEVEL_NODES), Some("gaps"));
    assert_eq!(closest("raduis", TOP_LEVEL_NODES), Some("radius"));
    assert_eq!(closest("bind", TOP_LEVEL_NODES), Some("binds"));
    assert_eq!(closest("qqqq", TOP_LEVEL_NODES), None);
    assert_eq!(edit_distance("kitten", "sitting"), 3);
    assert_eq!(edit_distance("", "abc"), 3);
    assert_eq!(edit_distance("same", "same"), 0);
}

#[test]
fn radius_rejects_bad_values() {
    assert!(Config::parse("radius").is_err());
    assert!(Config::parse("radius -1").is_err());
    assert!(Config::parse(&format!("radius {}", MAX_RADIUS + 1)).is_err());
    assert!(Config::parse("radius 2.5").is_err());
    assert!(Config::parse("radius \"round\"").is_err());
    assert!(Config::parse("radius 4 6").is_err());
}

#[test]
fn blur_is_off_without_a_block_and_on_with_one() {
    assert!(Config::default().blur().off);
    let blur = Config::parse("blur {}").unwrap().blur();
    assert!(!blur.off);
    assert_eq!((blur.passes, blur.offset), (3, 3.0));
}

#[test]
fn blur_settings_are_read() {
    let blur = Config::parse("blur { passes 5; offset 2.5; }")
        .unwrap()
        .blur();
    assert_eq!((blur.passes, blur.offset), (5, 2.5));
    assert_eq!(
        Config::parse("blur { offset 2; }").unwrap().blur().offset,
        2.0
    );
    assert!(Config::parse("blur { off; passes 4; }").unwrap().blur().off);
    assert!(!Config::parse("blur {}").unwrap().blur().all_windows);
    assert!(
        Config::parse("blur { all-windows; }")
            .unwrap()
            .blur()
            .all_windows
    );
    assert!(Config::parse("blur { all-windows 3; }").is_err());
    assert!(!Config::parse("blur {}").unwrap().blur().all_layers);
    assert!(
        Config::parse("blur { all-layers; }")
            .unwrap()
            .blur()
            .all_layers
    );
    assert!(Config::parse("blur {}")
        .unwrap()
        .blur_ignore_layers()
        .is_empty());
    let cfg =
        Config::parse("blur { ignore-layer \"Nbar-Top\"; ignore-layer \"wallpaper\"; }").unwrap();
    assert_eq!(cfg.blur_ignore_layers(), ["Nbar-Top", "wallpaper"]);
    assert!(Config::parse("blur { ignore-layer; }").is_err());
    assert!(Config::parse("blur { ignore-layer 3; }").is_err());
    assert!(!Config::parse("blur {}").unwrap().blur().behind_windows);
    assert!(
        Config::parse("blur { behind-windows; }")
            .unwrap()
            .blur()
            .behind_windows
    );
}

#[test]
fn blur_rejects_bad_values() {
    assert!(Config::parse("blur { passes 0; }").is_err());
    assert!(Config::parse("blur { passes 32; }").is_err());
    assert!(Config::parse("blur { offset 0; }").is_err());
    assert!(Config::parse("blur { offset \"x\"; }").is_err());
    assert!(Config::parse("blur { radius 3; }").is_err());
}

#[test]
fn snap_actions_are_parsed() {
    let config = Config::parse(
        r#"binds {
                "Alt+Left" { snap-left; }
                "Alt+Right" { snap-right; }
                "Alt+Up" { snap-top; }
                "Alt+Down" { snap-bottom; }
            }"#,
    )
    .unwrap();
    let alt = mods(false, true, false, false);
    assert_eq!(
        config.action_for(&alt, Keysym::Left),
        Some(&Action::Snap(Side::Left))
    );
    assert_eq!(
        config.action_for(&alt, Keysym::Right),
        Some(&Action::Snap(Side::Right))
    );
    assert_eq!(
        config.action_for(&alt, Keysym::Up),
        Some(&Action::Snap(Side::Top))
    );
    assert_eq!(
        config.action_for(&alt, Keysym::Down),
        Some(&Action::Snap(Side::Bottom))
    );
    assert!(Config::parse("binds { \"Alt+Left\" { snap-left 1; } }").is_err());
}

#[test]
fn switcher_defaults_to_a_third_of_the_screen_and_a_short_slide() {
    let config = Config::default();
    assert_eq!(config.switcher().thumbnail_height, None);
    assert_eq!(config.switcher().slide_ms, DEFAULT_SWITCHER_SLIDE_MS);
}

#[test]
fn switcher_settings_are_parsed() {
    let config = Config::parse("switcher { thumbnail-height 240; slide-ms 200; }").unwrap();
    assert_eq!(config.switcher().thumbnail_height, Some(240));
    assert_eq!(config.switcher().slide_ms, 200);
    let config = Config::parse("switcher { slide-ms 0; }").unwrap();
    assert_eq!(config.switcher().thumbnail_height, None);
    assert_eq!(config.switcher().slide_ms, 0);
}

#[test]
fn overview_selection_defaults_to_the_old_look() {
    let config = Config::default();
    assert_eq!(config.overview().selection_width, DEFAULT_SELECTION_WIDTH);
    assert_eq!(config.overview().selection_color, DEFAULT_SELECTION_COLOR);
}

#[test]
fn overview_selection_is_parsed() {
    let config =
        Config::parse("overview { selection-width 6; selection-color \"#ff0000\"; }").unwrap();
    assert_eq!(config.overview().selection_width, 6);
    assert_eq!(config.overview().selection_color, [1.0, 0.0, 0.0, 1.0]);
    // One setting alone leaves the other at its default.
    let config = Config::parse("overview { selection-width 0; }").unwrap();
    assert_eq!(config.overview().selection_width, 0);
    assert_eq!(config.overview().selection_color, DEFAULT_SELECTION_COLOR);
}

#[test]
fn overview_rejects_bad_values() {
    assert!(Config::parse("overview { selection-width -1; }").is_err());
    assert!(Config::parse("overview { selection-width 1000; }").is_err());
    assert!(Config::parse("overview { selection-color \"red\"; }").is_err());
    assert!(Config::parse("overview { nonsense 1; }").is_err());
}

#[test]
fn switcher_rejects_bad_values() {
    assert!(Config::parse("switcher { thumbnail-height 10; }").is_err());
    assert!(Config::parse("switcher { thumbnail-height 5000; }").is_err());
    assert!(Config::parse("switcher { thumbnail-height \"big\"; }").is_err());
    assert!(Config::parse("switcher { slide-ms 5000; }").is_err());
    assert!(Config::parse("switcher { slide-ms -1; }").is_err());
    assert!(Config::parse("switcher { nonsense 1; }").is_err());
}

#[test]
fn hot_corner_is_off_unless_configured() {
    assert_eq!(Config::default().hot_corner(), None);
    assert_eq!(Config::parse("hot-corner 0").unwrap().hot_corner(), None);
    assert_eq!(
        Config::parse("hot-corner { size 0; dwell-ms 100; }")
            .unwrap()
            .hot_corner(),
        None
    );
}

#[test]
fn hot_corner_settings_are_parsed() {
    let config = Config::parse(
        "hot-corner { size 4; dwell-ms 250; monitor \"DP-1\"; monitor \"HDMI-A-1\"; }",
    )
    .unwrap();
    assert_eq!(
        config.hot_corner(),
        Some(HotCorner {
            size: 4,
            dwell: Duration::from_millis(250),
            monitors: vec!["DP-1".to_owned(), "HDMI-A-1".to_owned()],
        })
    );
}

#[test]
fn hot_corner_block_defaults_to_on_for_every_monitor() {
    let hot = Config::parse("hot-corner { dwell-ms 100; }")
        .unwrap()
        .hot_corner()
        .unwrap();
    assert_eq!(hot.size, 4);
    assert_eq!(hot.dwell, Duration::from_millis(100));
    assert!(hot.monitors.is_empty());
    let hot = Config::parse("hot-corner 8").unwrap().hot_corner().unwrap();
    assert_eq!(hot.size, 8);
    assert_eq!(hot.dwell, Duration::from_millis(250));
}

#[test]
fn hot_corner_rejects_bad_values() {
    assert!(Config::parse("hot-corner { size 65; }").is_err());
    assert!(Config::parse("hot-corner { size -1; }").is_err());
    assert!(Config::parse("hot-corner { dwell-ms 5001; }").is_err());
    assert!(Config::parse("hot-corner { monitor 3; }").is_err());
    assert!(Config::parse("hot-corner { nonsense 1; }").is_err());
    assert!(Config::parse("hot-corner 65").is_err());
    // The limits themselves are fine.
    assert!(Config::parse("hot-corner { size 64; dwell-ms 5000; }").is_ok());
}

#[test]
fn hot_corner_is_a_known_top_level_node() {
    assert!(TOP_LEVEL_NODES.contains(&"hot-corner"));
}

#[test]
fn workspaces_are_shared_unless_per_output() {
    assert!(!Config::default().workspaces_per_output());
    assert!(!Config::parse("workspaces 3")
        .unwrap()
        .workspaces_per_output());
}

#[test]
fn workspaces_block_sets_per_output_and_count() {
    let config = Config::parse("workspaces { per-output; }").unwrap();
    assert!(config.workspaces_per_output());
    assert_eq!(config.workspaces(), DEFAULT_WORKSPACES);
    let config = Config::parse("workspaces { count 4; per-output; }").unwrap();
    assert!(config.workspaces_per_output());
    assert_eq!(config.workspaces(), 4);
    let config = Config::parse("workspaces 5 { per-output #true; }").unwrap();
    assert!(config.workspaces_per_output());
    assert_eq!(config.workspaces(), 5);
    let config = Config::parse("workspaces { count 2; per-output #false; }").unwrap();
    assert!(!config.workspaces_per_output());
    assert_eq!(config.workspaces(), 2);
}

#[test]
fn workspaces_block_rejects_bad_values() {
    assert!(Config::parse("workspaces { count 0; }").is_err());
    assert!(Config::parse("workspaces { count 65; }").is_err());
    assert!(Config::parse("workspaces { count; }").is_err());
    assert!(Config::parse("workspaces { per-output 3; }").is_err());
    assert!(Config::parse("workspaces { nonsense 1; }").is_err());
}

#[test]
fn forward_key_takes_one_app_id() {
    let config =
        Config::parse(r#"binds { Super+F10 { forward-key "com.obsproject.Studio"; } }"#).unwrap();
    assert_eq!(
        config.action_for(&mods(false, false, false, true), Keysym::F10),
        Some(&Action::ForwardKey("com.obsproject.Studio".to_string())),
    );
    assert!(Config::parse("binds { F10 { forward-key; } }").is_err());
    assert!(Config::parse(r#"binds { F10 { forward-key "a" "b"; } }"#).is_err());
}

#[test]
fn workspace_wall_is_off_unless_asked_for() {
    assert!(Config::default().workspace_wall().is_none());
    assert!(Config::parse("gaps 4").unwrap().workspace_wall().is_none());
}

#[test]
fn workspace_wall_bare_block_and_flag() {
    assert!(Config::parse("workspace-wall")
        .unwrap()
        .workspace_wall()
        .is_some());
    assert!(Config::parse("workspace-wall #false")
        .unwrap()
        .workspace_wall()
        .is_none());
    let wall = Config::parse("workspace-wall { height 120; }")
        .unwrap()
        .workspace_wall();
    assert_eq!(wall.map(|w| w.height), Some(120));
    assert!(Config::parse("workspace-wall { height 120; off; }")
        .unwrap()
        .workspace_wall()
        .is_none());
    assert!(Config::parse("workspace-wall { height 5; }").is_err());
    // the windows of each workspace are drawn in its square unless asked not to
    assert_eq!(wall.map(|w| w.thumbnails), Some(true));
    let plain = Config::parse("workspace-wall { thumbnails #false; }")
        .unwrap()
        .workspace_wall();
    assert_eq!(plain.map(|w| w.thumbnails), Some(false));
    assert!(Config::parse("workspace-wall { nope; }").is_err());
}

fn facts<'a>(app_id: &'a str, tag: Option<&'a str>) -> WindowFacts<'a> {
    WindowFacts {
        app_id,
        title: "",
        tag,
        dialog: false,
        restored: false,
    }
}

#[test]
fn window_rule_is_a_known_top_level_node() {
    assert!(TOP_LEVEL_NODES.contains(&"window-rule"));
    assert!(Config::default().window_rules().is_empty());
}

#[test]
fn window_rule_reads_matches_and_actions() {
    let config = Config::parse(
        r#"
window-rule {
    match app-id="firefox-developer-edition"
    match tag="browser"

    open-on-output "DP-1"
    open-workspace 1
    open-maximized #true
}
"#,
    )
    .unwrap();
    let rule = &config.window_rules()[0];
    assert_eq!(
        rule.matches,
        [
            WindowMatch::AppId("firefox-developer-edition".into()),
            WindowMatch::Tag("browser".into())
        ]
    );
    assert_eq!(rule.output, Some(RuleOutput::Named("DP-1".into())));
    assert_eq!(rule.workspace, Some(0));
    assert_eq!(rule.maximized, Some(true));
    assert_eq!(rule.size, None);
}

#[test]
fn window_rule_needs_every_match_line_to_hold() {
    let config = Config::parse(
        r#"window-rule { match app-id="ff"; match tag="browser"; open-workspace 2; }"#,
    )
    .unwrap();
    assert!(config
        .window_rule_for(&facts("ff", Some("browser")))
        .is_some());
    assert!(config
        .window_rule_for(&facts("ff", Some("other")))
        .is_none());
    assert!(config.window_rule_for(&facts("ff", None)).is_none());
    assert!(config
        .window_rule_for(&facts("other", Some("browser")))
        .is_none());
}

#[test]
fn window_rule_first_match_wins() {
    let config = Config::parse(
        r#"
window-rule { match app-id="ff"; open-workspace 3; }
window-rule { match app-id="ff"; open-workspace 4; }
window-rule { open-workspace 5; }
"#,
    )
    .unwrap();
    assert_eq!(
        config
            .window_rule_for(&facts("ff", None))
            .unwrap()
            .workspace,
        Some(2)
    );
    // A rule without match lines fits everything that no earlier rule took.
    assert_eq!(
        config
            .window_rule_for(&facts("zzz", None))
            .unwrap()
            .workspace,
        Some(4)
    );
}

#[test]
fn window_rule_matches_dialog_restored_and_title() {
    let config = Config::parse(
        r#"
window-rule { match dialog=#true; open-on-output "pointer"; open-size 900 700; }
window-rule { match restored=#true; open-workspace 1; }
window-rule { match title="Picture-in-Picture"; floating; }
"#,
    )
    .unwrap();
    let rules = config.window_rules();
    assert_eq!(rules[0].output, Some(RuleOutput::Pointer));
    assert_eq!(rules[0].size, Some((900, 700)));
    assert!(rules[2].floating);

    let dialog = WindowFacts {
        dialog: true,
        ..facts("x", None)
    };
    let restored = WindowFacts {
        restored: true,
        ..facts("x", None)
    };
    let pip = WindowFacts {
        title: "Firefox - Picture-in-Picture",
        ..facts("x", None)
    };
    assert_eq!(config.window_rule_for(&dialog), Some(&rules[0]));
    assert_eq!(config.window_rule_for(&restored), Some(&rules[1]));
    assert_eq!(config.window_rule_for(&pip), Some(&rules[2]));
    assert!(config.window_rule_for(&facts("x", None)).is_none());
}

#[test]
fn window_rule_dialog_false_matches_plain_windows() {
    let config = Config::parse(r#"window-rule { match dialog=#false; floating; }"#).unwrap();
    assert!(config.window_rule_for(&facts("x", None)).is_some());
    let dialog = WindowFacts {
        dialog: true,
        ..facts("x", None)
    };
    assert!(config.window_rule_for(&dialog).is_none());
}

#[test]
fn window_rule_says_when_it_places_the_window() {
    let config = Config::parse(
        r#"
window-rule { open-workspace 2; }
window-rule { open-size 480 270; }
window-rule { open-maximized #false; }
"#,
    )
    .unwrap();
    let rules = config.window_rules();
    assert!(!rules[0].places_window());
    assert!(rules[1].places_window());
    assert!(rules[2].places_window());
}

#[test]
fn window_rule_rejects_bad_input() {
    for bad in [
        r#"window-rule { match; }"#,
        r#"window-rule { match "firefox"; }"#,
        r#"window-rule { match colour="red"; }"#,
        r#"window-rule { match dialog="maybe"; }"#,
        r#"window-rule { match app-id=3; }"#,
        r#"window-rule { open-workspace 0; }"#,
        r#"window-rule { open-workspace 65; }"#,
        r#"window-rule { open-size 480; }"#,
        r#"window-rule { open-size 0 270; }"#,
        r#"window-rule { open-size 480 270 3; }"#,
        r#"window-rule { open-on-output; }"#,
        r#"window-rule { open-on-output ""; }"#,
        r#"window-rule { open-maximized 3; }"#,
        r#"window-rule { teleport 1; }"#,
        r#"window-rule { floating; open-maximized #true; }"#,
        r#"window-rule "firefox""#,
    ] {
        assert!(Config::parse(bad).is_err(), "{bad} should not parse");
    }
}

#[test]
fn check_config_warns_about_rules_that_do_nothing() {
    let warnings = Config::check_text(r#"window-rule { match app-id="x"; }"#).unwrap();
    assert!(
        warnings.iter().any(|w| w.contains("no actions")),
        "{warnings:?}"
    );

    let warnings = Config::check_text(
        r#"window-rule { match restored=#true; open-workspace 1; open-size 10 10; }"#,
    )
    .unwrap();
    assert!(
        warnings.iter().any(|w| w.contains("restored")),
        "{warnings:?}"
    );

    let warnings =
        Config::check_text(r#"window-rule { match restored=#true; open-workspace 1; }"#).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn color_representation_yuv_is_off_unless_asked_for() {
    assert!(!Config::default().color_representation_yuv());
    assert!(TOP_LEVEL_NODES.contains(&"color-representation"));
    let on = Config::parse("color-representation { yuv; }").unwrap();
    assert!(on.color_representation_yuv());
    let on = Config::parse("color-representation { yuv #true; }").unwrap();
    assert!(on.color_representation_yuv());
    let off = Config::parse("color-representation { yuv #false; }").unwrap();
    assert!(!off.color_representation_yuv());
    assert!(Config::parse("color-representation { hdr; }").is_err());
}

#[test]
fn allow_tearing_is_off_unless_asked_for() {
    assert!(!Config::default().allow_tearing());
    assert!(TOP_LEVEL_NODES.contains(&"allow-tearing"));
    assert!(Config::parse("allow-tearing #true")
        .unwrap()
        .allow_tearing());
    assert!(!Config::parse("allow-tearing #false")
        .unwrap()
        .allow_tearing());
}

#[test]
fn global_hotkeys_are_off_unless_asked_for() {
    assert!(!Config::default().global_hotkeys());
    assert!(TOP_LEVEL_NODES.contains(&"global-hotkeys"));
    assert!(Config::parse("global-hotkeys #true")
        .unwrap()
        .global_hotkeys());
    assert!(!Config::parse("global-hotkeys #false")
        .unwrap()
        .global_hotkeys());
}

#[test]
fn blur_window_filter() {
    let all = Config::parse("blur { all-windows; }").unwrap();
    assert!(all.blur_windows().allows("firefox"));

    let ignore =
        Config::parse("blur { all-windows; ignore-window \"firefox\"; ignore-window \"mpv\"; }")
            .unwrap();
    assert!(!ignore.blur_windows().allows("firefox"));
    assert!(!ignore.blur_windows().allows("mpv"));
    assert!(ignore.blur_windows().allows("kitty"));

    let only = Config::parse("blur { only-window \"kitty\"; }").unwrap();
    assert!(only.blur_windows().allows("kitty"));
    assert!(!only.blur_windows().allows("firefox"));

    let both = Config::parse("blur { only-window \"kitty\"; ignore-window \"kitty\"; }").unwrap();
    assert!(!both.blur_windows().allows("kitty"));

    let b = only.blur();
    assert!(only
        .blur_for_window(Some(b), "kitty")
        .is_some_and(|b| b.all_windows));
    assert!(!only.blur().all_windows);
    assert!(only.blur_for_window(Some(b), "firefox").is_none());

    assert!(Config::parse("blur { ignore-window; }").is_err());
    assert!(Config::parse("blur { only-window 3; }").is_err());
}

#[test]
fn log_file_is_off_by_default() {
    let log = Config::default().log().clone();
    assert!(!log.enable);
    assert_eq!(log.level, LogLevel::Info);
    assert_eq!(log.path, None);
    assert_eq!(DEFAULT_LOG_PATH, "/tmp/jff.log");
    assert_eq!(
        log.resolved_path(),
        std::path::PathBuf::from("/tmp/jff.log")
    );
    assert!(TOP_LEVEL_NODES.contains(&"log"));
}

#[test]
fn log_block_needs_enable() {
    // Settings without `enable` are kept but write nothing.
    let config = Config::parse(r#"log { level "debug"; }"#).unwrap();
    assert!(!config.log().enable);
    assert_eq!(config.log().level, LogLevel::Debug);
    // An empty block is not enabling either.
    assert!(!Config::parse("log").unwrap().log().enable);
    assert!(!Config::parse("log {}").unwrap().log().enable);
}

#[test]
fn log_enable_takes_a_bare_name_or_a_value() {
    assert!(Config::parse("log { enable; }").unwrap().log().enable);
    assert!(Config::parse("log { enable #true; }").unwrap().log().enable);
    assert!(
        Config::parse(r#"log { enable "on"; }"#)
            .unwrap()
            .log()
            .enable
    );
    assert!(
        !Config::parse("log { enable #false; }")
            .unwrap()
            .log()
            .enable
    );
    assert!(Config::parse("log { enable 3; }").is_err());
}

#[test]
fn log_level_is_info_unless_set() {
    let config = Config::parse("log { enable; }").unwrap();
    assert_eq!(config.log().level, LogLevel::Info);
    for (name, level) in [
        ("error", LogLevel::Error),
        ("warn", LogLevel::Warn),
        ("info", LogLevel::Info),
        ("debug", LogLevel::Debug),
        ("trace", LogLevel::Trace),
        ("DEBUG", LogLevel::Debug),
    ] {
        let text = format!(r#"log {{ enable; level "{name}"; }}"#);
        assert_eq!(Config::parse(&text).unwrap().log().level, level, "{name}");
    }
    assert!(Config::parse(r#"log { level "loud"; }"#).is_err());
    assert!(Config::parse("log { level 3; }").is_err());
    assert!(Config::parse("log { level; }").is_err());
    assert_eq!(LogLevel::Debug.to_string(), "debug");
}

#[test]
fn log_path_is_where_the_file_goes() {
    let config = Config::parse(r#"log { enable; path "/var/tmp/jff-debug.log"; }"#).unwrap();
    assert_eq!(config.log().path.as_deref(), Some("/var/tmp/jff-debug.log"));
    assert_eq!(
        config.log().resolved_path(),
        std::path::PathBuf::from("/var/tmp/jff-debug.log")
    );
    // An empty path is no path: the default is used.
    let config = Config::parse(r#"log { enable; path ""; }"#).unwrap();
    assert_eq!(config.log().path, None);
    assert_eq!(
        config.log().resolved_path(),
        std::path::PathBuf::from(DEFAULT_LOG_PATH)
    );
    assert!(Config::parse("log { path 3; }").is_err());
}

#[test]
fn unknown_log_settings_are_errors() {
    assert!(Config::parse("log { enabled; }").is_err());
    assert!(Config::parse(r#"log { file "/tmp/x"; }"#).is_err());
}

#[test]
fn check_config_warns_about_log_settings_that_do_nothing() {
    let warnings = Config::check_text(r#"log { level "debug"; }"#).unwrap();
    assert!(warnings.iter().any(|w| w.contains("log")), "{warnings:?}");
    assert!(Config::check_text(r#"log { enable; level "debug"; }"#)
        .unwrap()
        .is_empty());
    assert!(Config::check_text("log { enable; }").unwrap().is_empty());
}

#[test]
fn color_management_is_on_unless_turned_off() {
    let config = Config::default();
    assert!(!config.color_management().off);
    assert!(!config.color_management().fullscreen_passthrough);
    assert!(TOP_LEVEL_NODES.contains(&"color-management"));
    let off = Config::parse("color-management { off; }").unwrap();
    assert!(off.color_management().off);
    assert!(!off.color_management().fullscreen_passthrough);
    let passthrough = Config::parse("color-management { fullscreen-passthrough; }").unwrap();
    assert!(!passthrough.color_management().off);
    assert!(passthrough.color_management().fullscreen_passthrough);
    assert!(!config.color_management().convert_srgb);
    let convert = Config::parse("color-management { convert-srgb; }").unwrap();
    assert!(convert.color_management().convert_srgb);
    assert!(!convert.color_management().off);
    let kept = Config::parse("color-management { off #false; }").unwrap();
    assert!(!kept.color_management().off);
    assert!(Config::parse("color-management { hdr; }").is_err());
    // The per-monitor form is not a top-level setting, and is not silently ignored there.
    assert!(Config::parse(r#"color-management "off""#).is_err());
}

#[test]
fn a_monitor_can_turn_color_management_off() {
    let config = Config::parse(
        r#"outputs {
            output "DP-1" { color-management "off"; }
            output "DP-2" { color-management; }
            output "DP-3" { mode "1920x1080@60"; }
        }"#,
    )
    .unwrap();
    assert!(config.output("DP-1").unwrap().color_management_off);
    assert!(!config.output("DP-2").unwrap().color_management_off);
    assert!(!config.output("DP-3").unwrap().color_management_off);
    assert!(config.output("DP-4").is_none());
    assert!(Config::parse(r#"outputs { output "DP-1" { color-management "maybe"; } }"#).is_err());
}

#[test]
fn a_monitor_can_ask_for_hdr() {
    let config = Config::parse(
        r#"outputs {
            output "DP-1" { hdr; }
            output "DP-2" { hdr "off"; }
            output "DP-3" { hdr #true; }
            output "DP-4" { mode "1920x1080@60"; }
        }"#,
    )
    .unwrap();
    assert!(config.output("DP-1").unwrap().hdr);
    assert!(!config.output("DP-2").unwrap().hdr);
    assert!(config.output("DP-3").unwrap().hdr);
    assert!(!config.output("DP-4").unwrap().hdr);
    assert!(!config.output("DP-5").map_or(false, |c| c.hdr));
    assert!(Config::parse(r#"outputs { output "DP-1" { hdr "maybe"; } }"#).is_err());
    assert!(Config::parse(r#"outputs { output "DP-1" { hdr 1 2; } }"#).is_err());
}

#[test]
fn sdr_white_is_a_luminance_in_range() {
    let config = Config::parse(
        r#"outputs {
            output "DP-1" { hdr; sdr-white 160; }
            output "DP-2" { hdr; }
        }"#,
    )
    .unwrap();
    assert_eq!(config.output("DP-1").unwrap().sdr_white, Some(160));
    assert_eq!(config.output("DP-2").unwrap().sdr_white, None);
    for bad in ["0", "5", "1001", "-3", "\"bright\"", "1 2", ""] {
        let text = format!(r#"outputs {{ output "DP-1" {{ sdr-white {bad}; }} }}"#);
        assert!(Config::parse(&text).is_err(), "sdr-white {bad}");
    }
}

#[test]
fn hdr_peak_is_a_luminance_in_range() {
    let config = Config::parse(
        r#"outputs {
            output "DP-1" { hdr; hdr-peak 400; }
            output "DP-2" { hdr; }
            output "DP-3" { hdr; hdr-peak 100; }
            output "DP-4" { hdr; hdr-peak 10000; }
        }"#,
    )
    .unwrap();
    assert_eq!(config.output("DP-1").unwrap().hdr_peak, Some(400));
    assert_eq!(config.output("DP-2").unwrap().hdr_peak, None);
    assert_eq!(config.output("DP-3").unwrap().hdr_peak, Some(100));
    assert_eq!(config.output("DP-4").unwrap().hdr_peak, Some(10000));
    for bad in ["0", "99", "10001", "-400", "400.5", "\"bright\"", "1 2", ""] {
        let text = format!(r#"outputs {{ output "DP-1" {{ hdr-peak {bad}; }} }}"#);
        assert!(Config::parse(&text).is_err(), "hdr-peak {bad}");
    }
}

#[test]
fn the_luminance_settings_fail_the_same_way() {
    for (key, range) in [
        ("sdr-white", "from 10 to 1000"),
        ("hdr-peak", "from 100 to 10000"),
    ] {
        for bad in ["1", "\"x\"", "1 2", ""] {
            let text = format!(r#"outputs {{ output "DP-1" {{ {key} {bad}; }} }}"#);
            let message = format!("{:#}", Config::parse(&text).unwrap_err());
            assert!(message.contains(key), "{message}");
            assert!(message.contains(range), "{message}");
        }
    }
}

#[test]
fn a_later_luminance_replaces_an_earlier_one() {
    let config = Config::parse(
        r#"outputs { output "DP-1" { hdr; sdr-white 120; sdr-white 160; hdr-peak 300; hdr-peak 450; } }"#,
    )
    .unwrap();
    assert_eq!(config.output("DP-1").unwrap().sdr_white, Some(160));
    assert_eq!(config.output("DP-1").unwrap().hdr_peak, Some(450));
}

#[test]
fn check_warns_about_luminances_without_hdr() {
    let warnings = Config::check_text(
        r#"outputs {
            output "DP-1" { sdr-white 160; }
            output "DP-2" { hdr-peak 400; }
            output "DP-3" { hdr "off"; sdr-white 160; hdr-peak 400; }
        }"#,
    )
    .unwrap();
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings[0].contains("\"DP-1\"") && warnings[0].contains("sdr-white"));
    assert!(warnings[1].contains("\"DP-2\"") && warnings[1].contains("hdr-peak"));
    assert!(warnings[2].contains("sdr-white and hdr-peak"));
    assert!(warnings.iter().all(|w| w.contains("no `hdr`")));
}

#[test]
fn check_warns_about_hdr_on_a_dark_monitor() {
    let warnings = Config::check_text(r#"outputs { output "DP-1" { off; hdr; } }"#).unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("`off`"));
}

#[test]
fn check_warns_when_the_peak_leaves_no_room_for_sdr_white() {
    // Under 2 times the default SDR white of 203.
    let tight = Config::check_text(r#"outputs { output "DP-1" { hdr; hdr-peak 300; } }"#).unwrap();
    assert_eq!(tight.len(), 1, "{tight:?}");
    assert!(tight[0].contains("sdr-white 203") && tight[0].contains("hdr-peak 300"));
    // The same peak is fine once SDR white is lowered, and so is a brighter panel.
    let lowered =
        Config::check_text(r#"outputs { output "DP-1" { hdr; hdr-peak 300; sdr-white 120; } }"#)
            .unwrap();
    assert!(lowered.is_empty(), "{lowered:?}");
    let bright =
        Config::check_text(r#"outputs { output "DP-1" { hdr; hdr-peak 1000; } }"#).unwrap();
    assert!(bright.is_empty(), "{bright:?}");
}

#[test]
fn check_is_quiet_about_a_complete_hdr_setup() {
    let text = r#"outputs { output "DP-1" { hdr; sdr-white 120; hdr-peak 400; } }"#;
    assert!(Config::check_text(text).unwrap().is_empty());
    assert!(Config::check_text(r#"outputs { output "DP-1" { hdr; } }"#)
        .unwrap()
        .is_empty());
}

#[test]
fn hdr_tone_map_takes_a_known_curve() {
    let config = Config::parse(
        r#"outputs {
            output "DP-1" { hdr; hdr-tone-map "bt2390"; }
            output "DP-2" { hdr; hdr-tone-map "knee"; }
            output "DP-3" { hdr; }
        }"#,
    )
    .unwrap();
    assert_eq!(
        config.output("DP-1").unwrap().hdr_tone_map,
        Some(ToneMap::Bt2390)
    );
    assert_eq!(
        config.output("DP-2").unwrap().hdr_tone_map,
        Some(ToneMap::Knee)
    );
    assert_eq!(config.output("DP-3").unwrap().hdr_tone_map, None);
}

#[test]
fn hdr_tone_map_fails_with_the_names_it_takes() {
    for bad in [
        "\"linear\"",
        "\"BT2390\"",
        "3",
        "#true",
        "\"knee\" \"bt2390\"",
        "",
    ] {
        let text = format!(r#"outputs {{ output "DP-1" {{ hdr-tone-map {bad}; }} }}"#);
        let message = format!("{:#}", Config::parse(&text).unwrap_err());
        assert!(message.contains("hdr-tone-map"), "{message}");
        assert!(
            message.contains("\"knee\"") && message.contains("\"bt2390\""),
            "{message}"
        );
    }
}

#[test]
fn a_later_hdr_tone_map_replaces_an_earlier_one() {
    let config = Config::parse(
        r#"outputs { output "DP-1" { hdr; hdr-tone-map "bt2390"; hdr-tone-map "knee"; } }"#,
    )
    .unwrap();
    assert_eq!(
        config.output("DP-1").unwrap().hdr_tone_map,
        Some(ToneMap::Knee)
    );
}

#[test]
fn check_warns_about_a_tone_map_without_hdr() {
    let warnings = Config::check_text(
        r#"outputs {
            output "DP-1" { hdr-tone-map "bt2390"; }
            output "DP-2" { hdr "off"; hdr-peak 400; hdr-tone-map "knee"; }
            output "DP-3" { hdr; hdr-tone-map "bt2390"; }
        }"#,
    )
    .unwrap();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("\"DP-1\"") && warnings[0].contains("hdr-tone-map"));
    assert!(warnings[1].contains("hdr-peak and hdr-tone-map"));
    assert!(warnings.iter().all(|w| w.contains("no `hdr`")));
}

#[test]
fn check_is_quiet_about_a_tone_map_with_hdr() {
    let text =
        r#"outputs { output "DP-1" { hdr; sdr-white 120; hdr-peak 400; hdr-tone-map "bt2390"; } }"#;
    assert!(Config::check_text(text).unwrap().is_empty());
}

#[test]
fn hdr_content_metadata_is_a_flag_that_is_on_when_not_set() {
    let config = Config::parse(
        r#"outputs {
            output "DP-1" { hdr; hdr-content-metadata #false; }
            output "DP-2" { hdr; hdr-content-metadata; }
            output "DP-3" { hdr; hdr-content-metadata "off"; }
            output "DP-4" { hdr; }
        }"#,
    )
    .unwrap();
    assert_eq!(
        config.output("DP-1").unwrap().hdr_content_metadata,
        Some(false)
    );
    assert_eq!(
        config.output("DP-2").unwrap().hdr_content_metadata,
        Some(true)
    );
    assert_eq!(
        config.output("DP-3").unwrap().hdr_content_metadata,
        Some(false)
    );
    assert_eq!(config.output("DP-4").unwrap().hdr_content_metadata, None);
}

#[test]
fn hdr_content_metadata_fails_on_a_value_that_is_not_a_flag() {
    for bad in ["3", "\"maybe\"", "#true #false"] {
        let text = format!(r#"outputs {{ output "DP-1" {{ hdr-content-metadata {bad}; }} }}"#);
        let message = format!("{:#}", Config::parse(&text).unwrap_err());
        assert!(message.contains("hdr-content-metadata"), "{message}");
    }
}

#[test]
fn check_warns_about_content_metadata_without_hdr_and_is_quiet_with_it() {
    let warnings =
        Config::check_text(r#"outputs { output "DP-1" { hdr-content-metadata #false; } }"#)
            .unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("hdr-content-metadata") && warnings[0].contains("no `hdr`"));
    let quiet = r#"outputs { output "DP-1" { hdr; hdr-content-metadata #false; } }"#;
    assert!(Config::check_text(quiet).unwrap().is_empty());
}
