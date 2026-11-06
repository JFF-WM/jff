//! Tests for `jff_wm::render::effects::border`.

use std::time::Duration;

use jff_wm::config::{Border, BorderAnimation};
use jff_wm::render::effects::border::{
    amount_target, band_rects, fade_amount, fade_color, mix, phase,
};

const A: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const B: [f32; 4] = [1.0, 0.5, 0.25, 0.0];

#[test]
fn mix_hits_both_ends_and_the_middle() {
    assert_eq!(mix(A, B, 0.0), A);
    assert_eq!(mix(A, B, 1.0), B);
    assert_eq!(mix(A, B, 0.5), [0.5, 0.25, 0.125, 0.5]);
}

#[test]
fn fade_runs_from_the_old_colour_to_the_new_one() {
    let total = Duration::from_millis(100);
    assert_eq!(fade_color(A, B, Duration::ZERO, total), A);
    assert_eq!(fade_color(A, B, total, total), B);
    // Past the end it stays put.
    assert_eq!(fade_color(A, B, total * 3, total), B);
    // Halfway in time is further than halfway in colour: it eases out.
    let half = fade_color(A, B, total / 2, total);
    assert!(half[0] > 0.5 && half[0] < 1.0, "{half:?}");
}

#[test]
fn no_fade_is_instant() {
    assert_eq!(fade_color(A, B, Duration::ZERO, Duration::ZERO), B);
}

#[test]
fn animation_fades_in_and_out_and_ends_exactly() {
    let total = Duration::from_millis(100);
    // Gaining focus: from nothing to everything.
    assert_eq!(fade_amount(0.0, 1.0, Duration::ZERO, total), 0.0);
    assert_eq!(fade_amount(0.0, 1.0, total, total), 1.0);
    let half = fade_amount(0.0, 1.0, total / 2, total);
    assert!(half > 0.5 && half < 1.0, "{half}");
    // Losing it: ends on exactly 0.0, which is what makes the ring static again.
    assert_eq!(fade_amount(1.0, 0.0, total, total), 0.0);
    assert_eq!(fade_amount(0.37, 0.0, total * 2, total), 0.0);
    assert_eq!(fade_amount(0.37, 1.0, total * 2, total), 1.0);
    // Changing focus mid-fade starts from where it got to: no jump.
    let mid = fade_amount(0.0, 1.0, total / 4, total);
    assert_eq!(fade_amount(mid, 0.0, Duration::ZERO, total), mid);
    // No fade time: at once.
    assert_eq!(fade_amount(0.0, 1.0, Duration::ZERO, Duration::ZERO), 1.0);
    assert_eq!(fade_amount(1.0, 0.0, Duration::ZERO, Duration::ZERO), 0.0);
}

#[test]
fn only_the_focused_animated_border_has_a_target() {
    let mut style = Border {
        width: 2,
        animate: BorderAnimation::Spin,
        focus_color_2: Some([1.0, 0.0, 0.0, 1.0]),
        ..Border::default()
    };
    assert_eq!(amount_target(&style, true), 1.0);
    assert_eq!(amount_target(&style, false), 0.0);
    style.animate = BorderAnimation::None;
    assert_eq!(amount_target(&style, true), 0.0);
    style.animate = BorderAnimation::Spin;
    style.period_ms = 0;
    assert_eq!(amount_target(&style, true), 0.0);
    style.period_ms = 3000;
    style.focus_color_2 = None;
    assert_eq!(amount_target(&style, true), 0.0);
}

#[test]
fn phase_starts_at_zero_and_wraps_at_the_period() {
    let ms = Duration::from_millis;
    assert_eq!(phase(ms(0), 3000), 0.0);
    assert_eq!(phase(ms(1500), 3000), 0.5);
    assert_eq!(phase(ms(750), 3000), 0.25);
    // A full turn is the start again, however many turns have gone by.
    assert_eq!(phase(ms(3000), 3000), 0.0);
    assert_eq!(phase(ms(6000), 3000), 0.0);
    assert_eq!(phase(ms(7500), 3000), 0.5);
}

#[test]
fn phase_stays_small_and_exact_after_a_very_long_time() {
    // 400 days of uptime: the wrap happens on integers, so nothing is lost.
    let long = Duration::from_secs(400 * 24 * 3600) + Duration::from_millis(1500);
    assert_eq!(phase(long, 3000), 0.5);
    let p = phase(long + Duration::from_millis(1), 3000);
    assert!((0.0..1.0).contains(&p), "{p}");
}

#[test]
fn phase_is_zero_without_a_period_and_never_nan_or_one() {
    let ms = Duration::from_millis;
    assert_eq!(phase(ms(1234), 0), 0.0);
    assert_eq!(phase(Duration::ZERO, 0), 0.0);
    for period in [1, 2, 3, 1000, 3000, 600_000] {
        for elapsed in [0, 1, period - 1, period, period + 1, 123_456_789] {
            let p = phase(ms(elapsed), period);
            assert!(
                p.is_finite() && (0.0..1.0).contains(&p),
                "{elapsed}/{period}: {p}"
            );
        }
    }
}

/// The ring's coverage as the shader computes it (`ring.frag`), for pixel centre `(x, y)`
/// of an element `w` by `h` pixels.
fn ring_coverage(x: f32, y: f32, w: i32, h: i32, border: i32, radius: i32) -> f32 {
    fn rounded_box(p: (f32, f32), half: (f32, f32), r: f32) -> f32 {
        let q = (p.0.abs() - (half.0 - r), p.1.abs() - (half.1 - r));
        let outside = (q.0.max(0.0).powi(2) + q.1.max(0.0).powi(2)).sqrt();
        outside + q.0.max(q.1).min(0.0) - r
    }
    let (w, h, border, radius) = (w as f32, h as f32, border as f32, radius as f32);
    let p = (x - 0.5 * w, y - 0.5 * h);
    let outer_half = (0.5 * w, 0.5 * h);
    let outer_r = if radius > 0.0 {
        (radius + border).min(outer_half.0.min(outer_half.1))
    } else {
        0.0
    };
    let outer = (0.5 - rounded_box(p, outer_half, outer_r)).clamp(0.0, 1.0);
    let inner_half = (
        (outer_half.0 - border - 1.0).max(0.0),
        (outer_half.1 - border - 1.0).max(0.0),
    );
    let inner_r = (radius - 1.0).min(inner_half.0.min(inner_half.1)).max(0.0);
    let inner = (0.5 - rounded_box(p, inner_half, inner_r)).clamp(0.0, 1.0);
    outer * (1.0 - inner)
}

/// Whether pixel `(x, y)` lies in one of `rects`.
fn within(rects: &[(i32, i32, i32, i32)], x: i32, y: i32) -> bool {
    rects
        .iter()
        .any(|&(rx, ry, rw, rh)| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
}

#[test]
fn damage_bands_cover_every_pixel_of_the_ring() {
    for (win_w, win_h, border, radius) in [
        (100, 60, 2, 8),
        (400, 300, 3, 20),
        (400, 300, 6, 0),
        (50, 30, 4, 40),
        (7, 5, 2, 3),
        (300, 200, 1, 64),
        (1, 1, 1, 0),
        (3, 200, 2, 30),
    ] {
        let (w, h) = (win_w + 2 * border, win_h + 2 * border);
        let bands = band_rects((w, h), border + 1, radius + border + 1);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                if ring_coverage(fx, fy, w, h, border, radius) > 0.0 {
                    assert!(
                        within(&bands, x, y),
                        "pixel {x},{y} of {w}x{h} (border {border}, radius {radius})"
                    );
                }
            }
        }
    }
}

#[test]
fn damage_bands_leave_the_window_alone_and_never_overlap() {
    let (w, h) = (404, 304);
    let bands = band_rects((w, h), 3, 23);
    let area: i32 = bands.iter().map(|&(_, _, bw, bh)| bw * bh).sum();
    // Far less than the whole element.
    assert!(area < (w * h) / 5, "{area}");
    for (i, a) in bands.iter().enumerate() {
        assert!(
            a.0 >= 0 && a.1 >= 0 && a.0 + a.2 <= w && a.1 + a.3 <= h,
            "{a:?}"
        );
        for b in &bands[i + 1..] {
            let apart =
                a.0 + a.2 <= b.0 || b.0 + b.2 <= a.0 || a.1 + a.3 <= b.1 || b.1 + b.3 <= a.1;
            assert!(apart, "{a:?} overlaps {b:?}");
        }
    }
    // The centre of the window is not damaged.
    assert!(!within(&bands, w / 2, h / 2));
    assert!(band_rects((0, 10), 3, 5).is_empty());
}
