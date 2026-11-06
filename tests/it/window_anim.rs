//! Tests for `jff_wm::feature::window_anim`.

use std::time::{Duration, Instant};

use jff_wm::config::Config;
use jff_wm::feature::window_anim::{open_alpha, OPEN};

#[test]
fn a_window_without_a_show_time_is_opaque() {
    assert_eq!(open_alpha(None, Instant::now()), 1.0);
}

#[test]
fn the_open_fade_runs_from_clear_to_opaque() {
    let shown = Instant::now();
    assert_eq!(open_alpha(Some(shown), shown), 0.0);
    let half = open_alpha(Some(shown), shown + OPEN / 2);
    assert!(half > 0.0 && half < 1.0, "{half}");
    assert_eq!(open_alpha(Some(shown), shown + OPEN), 1.0);
    assert_eq!(open_alpha(Some(shown), shown + OPEN * 10), 1.0);
}

#[test]
fn the_open_fade_never_goes_backwards() {
    let shown = Instant::now();
    let mut last = 0.0;
    for ms in 0..=OPEN.as_millis() as u64 + 20 {
        let alpha = open_alpha(Some(shown), shown + Duration::from_millis(ms));
        assert!(alpha >= last, "{ms} ms: {alpha} < {last}");
        last = alpha;
    }
}

#[test]
fn window_animations_are_on_unless_disabled() {
    assert!(Config::default().window_animations());
    assert!(Config::parse("window-animations #true")
        .unwrap()
        .window_animations());
    assert!(!Config::parse("window-animations #false")
        .unwrap()
        .window_animations());
    assert!(Config::parse("window-animations 3").is_err());
}
