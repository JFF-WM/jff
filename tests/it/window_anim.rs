//! Tests for `jff_wm::feature::window_anim`.

use std::time::{Duration, Instant};

use jff_wm::config::{Config, OpenStyle};
use jff_wm::feature::window_anim::{open_alpha, open_frame, OpenFrame};

const OPEN: Duration = Duration::from_millis(100);

#[test]
fn a_window_without_a_show_time_is_opaque() {
    assert_eq!(open_alpha(None, Instant::now(), OPEN), 1.0);
}

#[test]
fn the_open_fade_runs_from_clear_to_opaque() {
    let shown = Instant::now();
    assert_eq!(open_alpha(Some(shown), shown, OPEN), 0.0, "open={OPEN:?}");
    let half = open_alpha(Some(shown), shown + OPEN / 2, OPEN);
    assert!(half > 0.0 && half < 1.0, "{half}");
    assert_eq!(
        open_alpha(Some(shown), shown + OPEN, OPEN),
        1.0,
        "open={OPEN:?}"
    );
    assert_eq!(
        open_alpha(Some(shown), shown + OPEN * 10, OPEN),
        1.0,
        "open={OPEN:?}"
    );
}

#[test]
fn the_open_fade_never_goes_backwards() {
    let shown = Instant::now();
    let mut last = 0.0;
    for ms in 0..=OPEN.as_millis() as u64 + 20 {
        let alpha = open_alpha(Some(shown), shown + Duration::from_millis(ms), OPEN);
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

#[test]
fn the_block_form_sets_the_fade_times() {
    let config = Config::parse("window-animations {\n open-ms 3000\n close-ms 80\n}").unwrap();
    assert!(config.window_animations());
    assert_eq!(
        config.window_animation_times(),
        (Duration::from_millis(3000), Duration::from_millis(80))
    );
    assert!(!Config::parse("window-animations { off; }")
        .unwrap()
        .window_animations());
    assert!(Config::parse("window-animations { speed 3 }").is_err());
}

#[test]
fn open_styles_move_or_scale_and_end_settled() {
    let shown = Instant::now();
    let early = shown + OPEN / 4;
    let zoom = open_frame(OpenStyle::Zoom, Some(shown), early, OPEN);
    assert!(zoom.scale < 1.0 && zoom.alpha < 1.0, "{zoom:?}");
    let slide = open_frame(OpenStyle::Slide, Some(shown), early, OPEN);
    assert!(slide.dy > 0 && slide.scale == 1.0, "{slide:?}");
    for style in [
        OpenStyle::Fade,
        OpenStyle::Zoom,
        OpenStyle::Slide,
        OpenStyle::Pop,
    ] {
        assert_eq!(
            open_frame(style, Some(shown), shown + OPEN, OPEN),
            OpenFrame::DONE
        );
    }
    assert_eq!(
        open_frame(OpenStyle::None, Some(shown), shown, OPEN),
        OpenFrame::DONE
    );
}

#[test]
fn the_open_style_is_read_from_the_block() {
    let config = Config::parse("window-animations {\n open \"zoom\"\n open-ms 350\n}").unwrap();
    assert_eq!(config.window_open_style(), OpenStyle::Zoom);
    assert_eq!(Config::default().window_open_style(), OpenStyle::Fade);
    assert!(Config::parse("window-animations { open \"spin\" }").is_err());
}
