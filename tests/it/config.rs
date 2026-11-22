//! The config nodes that decide how client-bound global hotkeys and `forward-key` fit together.

use jff_wm::config::{
    Action, Config, DEFAULT_CONTAINER_BACKGROUND_COLOR, DEFAULT_CONTAINER_BORDER_COLOR,
    DEFAULT_CONTAINER_BORDER_WIDTH, DEFAULT_CONTAINER_FONT_COLOR, DEFAULT_OVERVIEW_BORDER_WIDTH,
    DEFAULT_SELECTION_COLOR,
};
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
fn overview_container_background_has_a_default_and_takes_a_colour() {
    let default = Config::parse("").unwrap();
    assert_eq!(
        default.overview().container_background_color,
        DEFAULT_CONTAINER_BACKGROUND_COLOR
    );
    let config = Config::parse("overview { container-background-color \"#ff000080\"; }").unwrap();
    let [r, g, b, a] = config.overview().container_background_color;
    assert_eq!([r, g, b], [1.0, 0.0, 0.0]);
    assert!((a - 128.0 / 255.0).abs() < 1e-6);
    let config = Config::parse("overview { container-background-color \"#00000000\"; }").unwrap();
    assert_eq!(config.overview().container_background_color[3], 0.0);
}

#[test]
fn overview_container_background_rejects_a_bad_colour() {
    assert!(Config::parse("overview { container-background-color \"red\"; }").is_err());
    assert!(Config::parse("overview { container-background-color \"#12345\"; }").is_err());
    assert!(Config::parse("overview { container-background-color; }").is_err());
}

#[test]
fn overview_container_background_leaves_the_other_settings_alone() {
    let config = Config::parse(
        "overview { container-background-color \"#112233\"; selection-width 7; blur; }",
    )
    .unwrap();
    assert_eq!(config.overview().selection_width, 7);
    assert!(config.overview().blur);
    assert_eq!(config.overview().selection_color, DEFAULT_SELECTION_COLOR);
}

#[test]
fn overview_container_border_keeps_the_old_look_by_default() {
    let overview = *Config::parse("").unwrap().overview();
    assert_eq!(
        overview.container_border_width,
        DEFAULT_CONTAINER_BORDER_WIDTH
    );
    assert_eq!(
        overview.container_border_color,
        DEFAULT_CONTAINER_BORDER_COLOR
    );
    assert_eq!(overview.container_radius, None);
}

#[test]
fn overview_container_border_takes_a_width_a_colour_and_a_radius() {
    let config = Config::parse(
        "overview { container-border-width 5; container-border-color \"#ff000080\"; container-radius 12; }",
    )
    .unwrap();
    let overview = config.overview();
    assert_eq!(overview.container_border_width, 5);
    assert_eq!(overview.container_border_color[0], 1.0);
    assert!((overview.container_border_color[3] - 128.0 / 255.0).abs() < 1e-6);
    assert_eq!(overview.container_radius, Some(12));
    let config =
        Config::parse("overview { container-border-width 0; container-radius 0; }").unwrap();
    assert_eq!(config.overview().container_border_width, 0);
    assert_eq!(config.overview().container_radius, Some(0));
}

#[test]
fn overview_container_border_has_a_range_and_a_shape() {
    for line in [
        "container-border-width -1",
        "container-border-width 65",
        "container-border-width 1.5",
        "container-border-width",
        "container-border-color \"red\"",
        "container-border-color",
        "container-radius -1",
        "container-radius 257",
        "container-radius",
    ] {
        let text = format!("overview {{ {line}; }}");
        assert!(Config::parse(&text).is_err(), "{line}");
    }
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

#[test]
fn overview_container_font_color_has_a_default_and_takes_a_colour() {
    let default = Config::parse("").unwrap();
    assert_eq!(
        default.overview().container_font_color,
        DEFAULT_CONTAINER_FONT_COLOR
    );
    let config = Config::parse("overview { container-font-color \"#ff000080\"; }").unwrap();
    let [r, g, b, a] = config.overview().container_font_color;
    assert_eq!([r, g, b], [1.0, 0.0, 0.0]);
    assert!((a - 128.0 / 255.0).abs() < 1e-6);
    assert!(Config::parse("overview { container-font-color \"red\"; }").is_err());
    assert!(Config::parse("overview { container-font-color; }").is_err());
}

#[test]
fn overview_border_has_a_default_and_takes_a_width_and_a_colour() {
    let overview = *Config::parse("").unwrap().overview();
    assert_eq!(overview.border_width, DEFAULT_OVERVIEW_BORDER_WIDTH);
    assert_eq!(overview.border_color, None);
    let config = Config::parse("overview { border 3; border-color \"#ff000080\"; }").unwrap();
    assert_eq!(config.overview().border_width, 3);
    let [r, g, b, a] = config.overview().border_color.unwrap();
    assert_eq!([r, g, b], [1.0, 0.0, 0.0]);
    assert!((a - 128.0 / 255.0).abs() < 1e-6);
    let config = Config::parse("overview { border 0; }").unwrap();
    assert_eq!(config.overview().border_width, 0);
    for line in [
        "border -1",
        "border 65",
        "border",
        "border-color \"red\"",
        "border-color",
    ] {
        let text = format!("overview {{ {line}; }}");
        assert!(Config::parse(&text).is_err(), "{line}");
    }
}

#[test]
fn overview_backdrop_is_off_by_default_and_takes_a_colour() {
    assert_eq!(Config::parse("").unwrap().overview().backdrop_color, None);
    let config = Config::parse("overview { backdrop-color \"#000\"; }").unwrap();
    assert_eq!(config.overview().backdrop_color, Some([0.0, 0.0, 0.0, 1.0]));
    let config = Config::parse("overview { backdrop-color \"#00000080\"; }").unwrap();
    let [_, _, _, a] = config.overview().backdrop_color.unwrap();
    assert!((a - 128.0 / 255.0).abs() < 1e-6);
    assert!(Config::parse("overview { backdrop-color \"black\"; }").is_err());
    assert!(Config::parse("overview { backdrop-color; }").is_err());
}

#[test]
fn overview_search_colors_are_off_by_default_and_take_a_colour() {
    let overview = *Config::parse("").unwrap().overview();
    assert_eq!(overview.search_background_color, None);
    assert_eq!(overview.search_font_color, None);
    let config = Config::parse(
        "overview { search-background-color \"#00000080\"; search-font-color \"#fff\"; }",
    )
    .unwrap();
    let [_, _, _, a] = config.overview().search_background_color.unwrap();
    assert!((a - 128.0 / 255.0).abs() < 1e-6);
    assert_eq!(config.overview().search_font_color, Some([1.0; 4]));
    for line in [
        "search-background-color \"red\"",
        "search-background-color",
        "search-font-color \"red\"",
        "search-font-color",
    ] {
        let text = format!("overview {{ {line}; }}");
        assert!(Config::parse(&text).is_err(), "{line}");
    }
}

#[test]
fn switcher_shows_containers_whole_unless_told_not_to() {
    assert!(Config::parse("").unwrap().switcher().group_containers);
    let config = Config::parse("switcher { group-containers #false; }").unwrap();
    assert!(!config.switcher().group_containers);
    let config = Config::parse("switcher { group-containers; slide-ms 50; }").unwrap();
    assert!(config.switcher().group_containers);
    assert_eq!(config.switcher().slide_ms, 50);
    assert!(Config::parse("switcher { group-containers \"maybe\"; }").is_err());
}
