//! Tests for `jff_wm::config`.

use std::time::Duration;
use std::time::SystemTime;

use smithay::input::keyboard::xkb;
use smithay::input::keyboard::Keysym;
use smithay::input::keyboard::ModifiersState;

use jff_wm::config::{
    closest, edit_distance, parse_color, Action, BorderAnimation, Config, ConfigWatch, FileStamp,
    HotCorner, KeyboardConfig, ModeRequest, ScreenshotConfig, Vrr, DEFAULT_BORDER_FADE_MS,
    DEFAULT_BORDER_PERIOD_MS, DEFAULT_BORDER_WIDTH, DEFAULT_CONFIG, DEFAULT_REPEAT_DELAY,
    DEFAULT_REPEAT_RATE, DEFAULT_SWITCHER_SLIDE_MS, DEFAULT_WORKSPACES, MAX_BORDER_PERIOD_MS,
    MAX_BORDER_WIDTH, MAX_GAPS, MAX_RADIUS, TOP_LEVEL_NODES,
};
use jff_wm::desk::{Goto, Side};

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
    let warnings = Config::check_text("gap 10\nradius 8\nfoo 1").unwrap();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("\"gap\"") && warnings[0].contains("\"gaps\""));
    assert!(warnings[1].contains("\"foo\"") && !warnings[1].contains("did you mean"));
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
    assert_eq!(closest("foo", TOP_LEVEL_NODES), None);
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
    assert!(!Config::parse("workspaces 3").unwrap().workspaces_per_output());
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
