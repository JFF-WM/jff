//! `window-rule`: where a window opens (`open-position`, `open-centered`).

use jff_wm::config::{Config, RuleContentType, WindowFacts};

fn first_rule(text: &str) -> jff_wm::config::WindowRule {
    let config = Config::parse(text).unwrap();
    config.window_rules()[0].clone()
}

#[test]
fn open_position_is_read_as_x_and_y() {
    let rule = first_rule("window-rule { match app-id=\"a\"; open-position 40 80; }");
    assert_eq!(rule.position, Some((40, 80)));
    assert!(!rule.centered);
}

#[test]
fn open_centered_is_a_flag() {
    let rule = first_rule("window-rule { match app-id=\"a\"; open-centered; }");
    assert!(rule.centered);
    assert_eq!(rule.position, None);
    let off = first_rule("window-rule { match app-id=\"a\"; open-centered #false; }");
    assert!(!off.centered);
}

#[test]
fn a_rule_that_places_a_window_is_not_empty_and_keeps_it_floating() {
    for body in ["open-position 1 2", "open-centered"] {
        let rule = first_rule(&format!("window-rule {{ match app-id=\"a\"; {body}; }}"));
        assert!(rule.has_actions(), "{body}");
        assert!(rule.places_window(), "{body}");
        assert!(!rule.only_tags(), "{body}");
    }
}

#[test]
fn a_bad_position_is_refused() {
    for body in [
        "open-position",
        "open-position 5",
        "open-position 1 2 3",
        "open-position -1 4",
        "open-position 99999 4",
        "open-position \"a\" \"b\"",
    ] {
        let text = format!("window-rule {{ match app-id=\"a\"; {body}; }}");
        assert!(Config::parse(&text).is_err(), "{body}");
    }
}

#[test]
fn contradicting_placements_are_refused() {
    for body in [
        "open-position 1 2; open-centered",
        "open-centered; open-maximized #true",
        "open-position 1 2; open-maximized #true",
        "open-centered; open-in-container \"T\"",
        "open-position 1 2; open-in-container \"T\"",
    ] {
        let text = format!("window-rule {{ match app-id=\"a\"; {body}; }}");
        assert!(Config::parse(&text).is_err(), "{body}");
    }
}

#[test]
fn centered_may_come_with_a_size_a_monitor_and_an_opacity() {
    let rule = first_rule(
        "window-rule { match app-id=\"a\"; open-on-output \"DP-1\"; open-size 300 200; open-centered; opacity 0.9; }",
    );
    assert!(rule.centered);
    assert_eq!(rule.size, Some((300, 200)));
    assert_eq!(rule.opacity, Some(900));
}

// ---- always-on-top --------------------------------------------------------------------------

#[test]
fn always_on_top_is_a_flag() {
    let rule = first_rule("window-rule { match app-id=\"a\"; always-on-top; }");
    assert!(rule.always_on_top);
    let off = first_rule("window-rule { match app-id=\"a\"; always-on-top #false; }");
    assert!(!off.always_on_top);
    let unset = first_rule("window-rule { match app-id=\"a\"; opacity 0.9; }");
    assert!(!unset.always_on_top);
    assert!(Config::parse("window-rule { match app-id=\"a\"; always-on-top 3; }").is_err());
}

#[test]
fn a_rule_with_only_always_on_top_does_not_use_up_the_rule_a_window_gets() {
    let rule = first_rule("window-rule { match app-id=\"a\"; always-on-top; }");
    assert!(rule.has_actions());
    assert!(rule.only_tags());
    assert!(!rule.places_window());
    let config = Config::parse(
        "window-rule { match app-id=\"a\"; always-on-top; }\n\
         window-rule { match app-id=\"a\"; open-centered; }",
    )
    .unwrap();
    let a = WindowFacts {
        app_id: "a",
        ..WindowFacts::default()
    };
    assert!(config.always_on_top_for(&a));
    assert!(config.window_rule_for(&a).is_some_and(|rule| rule.centered));
    let b = WindowFacts {
        app_id: "b",
        ..WindowFacts::default()
    };
    assert!(!config.always_on_top_for(&b));
}

#[test]
fn always_on_top_may_come_with_a_placement() {
    let rule =
        first_rule("window-rule { match app-id=\"a\"; open-position 10 20; always-on-top; }");
    assert!(rule.always_on_top);
    assert_eq!(rule.position, Some((10, 20)));
    assert!(rule.places_window());
}

// ---- content-type ---------------------------------------------------------------------------

#[test]
fn content_type_is_game_video_photo_or_none() {
    let game = first_rule("window-rule { match app-id=\"a\"; content-type \"game\"; }");
    assert_eq!(game.content_type, Some(RuleContentType::Game));
    let video = first_rule("window-rule { match app-id=\"a\"; content-type \"video\"; }");
    assert_eq!(video.content_type, Some(RuleContentType::Video));
    let photo = first_rule("window-rule { match app-id=\"a\"; content-type \"photo\"; }");
    assert_eq!(photo.content_type, Some(RuleContentType::Photo));
    let none = first_rule("window-rule { match app-id=\"a\"; content-type \"none\"; }");
    assert_eq!(none.content_type, Some(RuleContentType::NoType));
    let unset = first_rule("window-rule { match app-id=\"a\"; opacity 0.9; }");
    assert_eq!(unset.content_type, None);
}

#[test]
fn a_bad_content_type_is_refused() {
    for body in [
        "content-type",
        "content-type \"Video\"",
        "content-type \"Game\"",
        "content-type \"cinema\"",
        "content-type 3",
        "content-type #true",
    ] {
        let text = format!("window-rule {{ match app-id=\"a\"; {body}; }}");
        assert!(Config::parse(&text).is_err(), "{body}");
    }
}

#[test]
fn a_rule_with_only_a_content_type_is_not_empty_and_does_not_place_the_window() {
    let rule = first_rule("window-rule { match app-id=\"a\"; content-type \"game\"; }");
    assert!(rule.has_actions());
    assert!(rule.only_tags());
    assert!(!rule.places_window());
}

#[test]
fn a_content_type_does_not_use_up_the_rule_a_window_gets() {
    // the window still gets the placement rule below it
    let config = Config::parse(
        "window-rule { match app-id=\"game\"; content-type \"game\"; }\n\
         window-rule { match app-id=\"game\"; open-centered; }",
    )
    .unwrap();
    let facts = WindowFacts {
        app_id: "game",
        ..WindowFacts::default()
    };
    assert_eq!(config.content_type_for(&facts), Some(RuleContentType::Game));
    assert!(config
        .window_rule_for(&facts)
        .is_some_and(|rule| rule.centered));
}

#[test]
fn the_first_matching_content_type_wins_and_others_get_none() {
    let config = Config::parse(
        "window-rule { match app-id=\"a\"; content-type \"none\"; }\n\
         window-rule { match app-id=\"a\"; content-type \"game\"; }\n\
         window-rule { match app-id=\"b\"; content-type \"game\"; }",
    )
    .unwrap();
    let of = |app_id| {
        config.content_type_for(&WindowFacts {
            app_id,
            ..WindowFacts::default()
        })
    };
    assert_eq!(of("a"), Some(RuleContentType::NoType));
    assert_eq!(of("b"), Some(RuleContentType::Game));
    assert_eq!(of("c"), None);
}
