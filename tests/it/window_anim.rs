//! Tests for `jff_wm::feature::window_anim`.

use std::time::{Duration, Instant};

use jff_wm::config::{Config, OpenStyle};
use jff_wm::feature::window_anim::{ghost_frame, open_alpha, open_frame, GhostMotion, OpenFrame};

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

#[test]
fn a_rule_opacity_does_not_unsettle_a_window() {
    let faded = OpenFrame::DONE.with_opacity(0.5);
    assert!(faded.settled());
    assert_eq!(faded.alpha, 1.0);
    assert_eq!(faded.opacity, 0.5);
    // A window that is still opening is not settled whatever its opacity.
    let shown = Instant::now();
    let opening = open_frame(OpenStyle::Fade, Some(shown), shown, OPEN).with_opacity(1.0);
    assert!(!opening.settled());
}

const WINDOW: (i32, i32, i32, i32) = (100, 80, 800, 600);
const BUTTON: (i32, i32, i32, i32) = (400, 1040, 160, 32);

fn rect_of(t: f32, motion: GhostMotion) -> (f32, (f64, f64, f64, f64)) {
    ghost_frame(motion, WINDOW, t)
}

fn as_f64(r: (i32, i32, i32, i32)) -> (f64, f64, f64, f64) {
    (r.0 as f64, r.1 as f64, r.2 as f64, r.3 as f64)
}

#[test]
fn a_closed_window_fades_out_where_it_was() {
    let (start, rect) = rect_of(0.0, GhostMotion::Stay);
    assert_eq!((start, rect), (1.0, as_f64(WINDOW)));
    let (end, rect) = rect_of(1.0, GhostMotion::Stay);
    assert_eq!((end, rect), (0.0, as_f64(WINDOW)));
}

#[test]
fn a_minimized_window_flies_into_the_button() {
    let motion = GhostMotion::Minimize(BUTTON);
    let (alpha, rect) = rect_of(0.0, motion);
    assert_eq!((alpha, rect), (1.0, as_f64(WINDOW)));
    let (alpha, rect) = rect_of(1.0, motion);
    assert_eq!((alpha, rect), (0.0, as_f64(BUTTON)));
    // On the way it is smaller than before and nearer the button, and still mostly there.
    let (alpha, rect) = rect_of(0.5, motion);
    assert!(alpha > 0.5 && alpha < 1.0, "{alpha}");
    assert!(rect.2 < 800.0 && rect.2 > 160.0, "{rect:?}");
    assert!(rect.1 > 80.0 && rect.1 < 1040.0, "{rect:?}");
}

#[test]
fn a_restored_window_flies_out_of_the_button() {
    let motion = GhostMotion::Restore(BUTTON);
    let (alpha, rect) = rect_of(0.0, motion);
    assert_eq!((alpha, rect), (0.0, as_f64(BUTTON)));
    let (alpha, rect) = rect_of(1.0, motion);
    assert_eq!((alpha, rect), (1.0, as_f64(WINDOW)));
}

#[test]
fn a_minimized_window_only_ever_shrinks_and_fades() {
    let motion = GhostMotion::Minimize(BUTTON);
    let (mut last_alpha, first) = rect_of(0.0, motion);
    let mut last_width = first.2;
    for step in 1..=20 {
        let (alpha, rect) = rect_of(step as f32 / 20.0, motion);
        assert!(alpha <= last_alpha, "step {step}: {alpha} > {last_alpha}");
        assert!(
            rect.2 <= last_width,
            "step {step}: {} > {last_width}",
            rect.2
        );
        last_alpha = alpha;
        last_width = rect.2;
    }
}

#[test]
fn a_ghost_frame_outside_zero_to_one_is_clamped() {
    let motion = GhostMotion::Minimize(BUTTON);
    assert_eq!(rect_of(-1.0, motion), rect_of(0.0, motion));
    assert_eq!(rect_of(2.0, motion), rect_of(1.0, motion));
}
