//! `Config::parse_lenient`: a part of the file that does not parse is left out, the rest stays.

use jff_wm::config::Config;

const FILE: &str = r#"
gaps 30
binds {
    Alt+Q { close; }
    Super+Shift+S { toggle-stats, }
}
outputs {
    output "DP-1" { hdr; }
    output "HDMI-A-2" { no-such-setting 1; }
}
window-rule { match app-id="a"; floating; }
window-rule { match app-id="b"; no-such-setting 1; }
radius 12
"#;

#[test]
fn the_strict_parse_still_fails_on_the_first_error() {
    assert!(Config::parse(FILE).is_err());
}

#[test]
fn only_the_bad_parts_are_left_out() {
    let config = Config::parse_lenient(FILE).unwrap();
    assert_eq!(config.gaps(), 30);
    assert_eq!(config.radius(), 12);
    assert!(config.output("DP-1").is_some_and(|output| output.hdr));
    assert!(config.output("HDMI-A-2").is_none());
    assert_eq!(config.window_rules().len(), 1);
    assert_eq!(config.skipped().len(), 3, "{:?}", config.skipped());
}

#[test]
fn what_was_left_out_is_named() {
    let config = Config::parse_lenient(FILE).unwrap();
    let skipped = config.skipped();
    assert!(skipped
        .iter()
        .any(|s| s.starts_with("bind \"Super+Shift+S\": ")));
    assert!(skipped
        .iter()
        .any(|s| s.starts_with("output \"HDMI-A-2\": ")));
    // numbered as in the file, the rule that was fine is number 1
    assert!(skipped.iter().any(|s| s.starts_with("window-rule 2: ")));
}

#[test]
fn a_bad_top_level_setting_leaves_the_others() {
    let config = Config::parse_lenient("gaps 30\nradius banana\nfocus-follows-mouse\n").unwrap();
    assert_eq!(config.gaps(), 30);
    assert!(config.focus_follows_mouse());
    assert_eq!(config.skipped().len(), 1);
    assert!(config.skipped()[0].starts_with("radius: "));
}

#[test]
fn a_good_file_leaves_nothing_out() {
    let clean = Config::parse_lenient("gaps 30\nbinds { Alt+Q { close; } }\n").unwrap();
    assert!(clean.skipped().is_empty());
    assert_eq!(clean.gaps(), 30);
    assert!(Config::parse("gaps 30").unwrap().skipped().is_empty());
}

#[test]
fn text_that_is_not_kdl_still_fails() {
    assert!(Config::parse_lenient("gaps {{{").is_err());
}

#[test]
fn focus_new_windows_defaults_on_and_can_be_turned_off() {
    assert!(Config::parse_lenient("gaps 30\n")
        .unwrap()
        .focus_new_windows());
    let config = Config::parse_lenient("focus-new-windows #false\n").unwrap();
    assert!(!config.focus_new_windows());
}
