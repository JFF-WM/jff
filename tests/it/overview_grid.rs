//! Tests for `jff_wm::primitives::overview_grid`.

use std::time::Duration;

use jff_wm::primitives::geom::Size;
use jff_wm::primitives::layout;
use jff_wm::primitives::layout::Rect;
use jff_wm::primitives::overview_grid::{
    anchor_in, arrange, cycle, dragged_thumb, drop_pos, ease_out_cubic, hit_test, lerp_thumb,
    modifier_released, moved_past, neighbor, progress, strip, Dir, Thumb,
};

const SCREEN: Rect = (0, 0, 1920, 1080);

fn thumb(x: i32, y: i32, w: i32, h: i32) -> Thumb {
    Thumb {
        pos: (x, y),
        size: (w, h),
        scale: 1.0,
    }
}

#[test]
fn nothing_to_arrange() {
    assert!(arrange(&[], SCREEN, 16).is_empty());
}

#[test]
fn a_small_window_is_centered_at_real_size() {
    let t = arrange(&[(400, 300)], SCREEN, 16);
    assert_eq!(t, vec![thumb(760, 390, 400, 300)]);
}

#[test]
fn a_big_window_shrinks_to_fit() {
    let t = arrange(&[(3840, 2160)], SCREEN, 16);
    assert_eq!(t[0].pos, (0, 0));
    assert_eq!(t[0].size, (1920, 1080));
    assert_eq!(t[0].scale, 0.5);
}

#[test]
fn windows_are_never_scaled_up() {
    let sizes: Vec<Size> = (0..7).map(|i| (100 + i * 50, 80 + i * 20)).collect();
    for t in arrange(&sizes, SCREEN, 16) {
        assert!(t.scale <= 1.0);
    }
}

#[test]
fn three_windows_make_two_columns_with_a_centered_last_row() {
    let t = arrange(&[(800, 600); 3], SCREEN, 16);
    assert_eq!(t[0].pos.1, t[1].pos.1);
    assert!(t[2].pos.1 > t[0].pos.1);
    let center_x = t[2].pos.0 as f64 + t[2].size.0 as f64 / 2.0;
    assert!((center_x - 960.0).abs() <= 1.0, "center was {center_x}");
}

#[test]
fn thumbnails_stay_inside_the_area_and_apart() {
    let area = (100, 50, 1600, 900);
    for n in 1..=12usize {
        let sizes: Vec<Size> = (0..n as i32)
            .map(|i| (300 + (i * 97) % 700, 200 + (i * 61) % 500))
            .collect();
        let thumbs = arrange(&sizes, area, 16);
        assert_eq!(thumbs.len(), n);
        for (i, t) in thumbs.iter().enumerate() {
            assert!(t.pos.0 >= area.0 && t.pos.1 >= area.1, "n={n} i={i}");
            assert!(t.pos.0 + t.size.0 <= area.0 + area.2 + 1, "n={n} i={i}");
            assert!(t.pos.1 + t.size.1 <= area.1 + area.3 + 1, "n={n} i={i}");
            for (j, u) in thumbs.iter().enumerate().skip(i + 1) {
                assert!(
                    !layout::intersects(t.rect(), u.rect()),
                    "n={n}: {i} overlaps {j}"
                );
            }
        }
    }
}

#[test]
fn degenerate_input_does_not_panic() {
    let t = arrange(&[(0, 0), (-5, 10)], (0, 0, 0, 0), 16);
    assert_eq!(t.len(), 2);
}

#[test]
fn hit_test_prefers_the_topmost() {
    let thumbs = [thumb(0, 0, 100, 100), thumb(50, 50, 100, 100)];
    assert_eq!(hit_test(&thumbs, (75.0, 75.0)), Some(1));
    assert_eq!(hit_test(&thumbs, (10.0, 10.0)), Some(0));
    assert_eq!(hit_test(&thumbs, (500.0, 500.0)), None);
    assert_eq!(hit_test(&thumbs[..1], (100.0, 50.0)), None);
}

#[test]
fn neighbor_walks_a_grid() {
    let g = [
        thumb(0, 0, 100, 100),
        thumb(200, 0, 100, 100),
        thumb(0, 200, 100, 100),
        thumb(200, 200, 100, 100),
    ];
    assert_eq!(neighbor(&g, 0, Dir::Right), Some(1));
    assert_eq!(neighbor(&g, 0, Dir::Down), Some(2));
    assert_eq!(neighbor(&g, 3, Dir::Left), Some(2));
    assert_eq!(neighbor(&g, 3, Dir::Up), Some(1));
    assert_eq!(neighbor(&g, 0, Dir::Left), None);
    assert_eq!(neighbor(&g, 0, Dir::Up), None);
    assert_eq!(neighbor(&g, 9, Dir::Up), None);
}

#[test]
fn neighbor_does_not_jump_rows_sideways() {
    let g = [
        thumb(0, 0, 100, 100),
        thumb(200, 0, 100, 100),
        thumb(100, 200, 100, 100),
    ];
    assert_eq!(neighbor(&g, 1, Dir::Right), None);
    assert_eq!(neighbor(&g, 1, Dir::Down), Some(2));
    assert!(neighbor(&g, 2, Dir::Up).is_some());
}

#[test]
fn neighbor_falls_back_outside_the_cone() {
    let g = [thumb(0, 0, 100, 100), thumb(100, 1000, 100, 100)];
    assert_eq!(neighbor(&g, 0, Dir::Right), Some(1));
}

#[test]
fn anchor_is_a_fraction_of_the_thumbnail() {
    let t = thumb(100, 200, 400, 300);
    assert_eq!(anchor_in(&t, (300.0, 350.0)), (0.5, 0.5));
    assert_eq!(anchor_in(&t, (0.0, 9999.0)), (0.0, 1.0));
}

#[test]
fn drag_starts_only_past_the_threshold() {
    assert!(!moved_past((10.0, 10.0), (13.0, 14.0), 8.0));
    assert!(moved_past((10.0, 10.0), (10.0, 18.0), 8.0));
    assert!(moved_past((10.0, 10.0), (0.0, 10.0), 8.0));
}

#[test]
fn dragged_thumbnail_keeps_the_grabbed_point_under_the_pointer() {
    let t = thumb(100, 200, 400, 300);
    let moved = dragged_thumb(t, (0.25, 0.5), (1000.0, 700.0));
    assert_eq!(moved.pos, (900, 550));
    assert_eq!(moved.size, t.size);
    assert_eq!(moved.scale, t.scale);
}

#[test]
fn dropped_window_keeps_the_grab_ratio_at_real_size() {
    let target: Rect = (1920, 0, 1280, 1024);
    assert_eq!(
        drop_pos((0.5, 0.25), (2560.0, 500.0), (800, 400), target),
        (2160, 400)
    );
}

#[test]
fn dropped_window_is_pulled_inside_the_monitor() {
    let target: Rect = (1920, 0, 1280, 1024);
    assert_eq!(
        drop_pos((0.5, 0.5), (1925.0, 5.0), (800, 400), target),
        (1920, 0)
    );
    assert_eq!(
        drop_pos((0.5, 0.5), (3199.0, 1020.0), (800, 400), target),
        (2400, 624)
    );
    assert_eq!(
        drop_pos((0.5, 0.5), (2500.0, 500.0), (5000, 5000), target),
        (1920, 0)
    );
}

#[test]
fn cycling_wraps() {
    assert_eq!(cycle(3, 0, false), 1);
    assert_eq!(cycle(3, 2, false), 0);
    assert_eq!(cycle(3, 0, true), 2);
    assert_eq!(cycle(1, 0, false), 0);
    assert_eq!(cycle(0, 0, true), 0);
}

#[test]
fn easing_runs_from_zero_to_one() {
    assert_eq!(ease_out_cubic(0.0), 0.0);
    assert_eq!(ease_out_cubic(1.0), 1.0);
    assert_eq!(ease_out_cubic(-3.0), 0.0);
    assert_eq!(ease_out_cubic(7.0), 1.0);
    assert!(ease_out_cubic(0.5) > 0.5);
}

#[test]
fn progress_is_clamped() {
    let total = Duration::from_millis(200);
    assert_eq!(progress(Duration::from_millis(100), total), 0.5);
    assert_eq!(progress(Duration::from_secs(5), total), 1.0);
    assert_eq!(progress(Duration::ZERO, Duration::ZERO), 1.0);
}

#[test]
fn lerp_thumb_goes_from_real_window_to_thumbnail() {
    let from = thumb(100, 100, 800, 600);
    let to = Thumb {
        pos: (0, 0),
        size: (400, 300),
        scale: 0.5,
    };
    assert_eq!(lerp_thumb(from, to, 0.0), from);
    assert_eq!(lerp_thumb(from, to, 1.0), to);
    let mid = lerp_thumb(from, to, 0.5);
    assert_eq!(mid.pos, (50, 50));
    assert_eq!(mid.size, (600, 450));
    assert_eq!(mid.scale, 0.75);
}

#[test]
fn nothing_to_strip() {
    assert!(strip(&[], SCREEN, 16, 360).is_empty());
}

#[test]
fn a_lone_window_in_the_strip_keeps_its_size() {
    let t = strip(&[(400, 300)], SCREEN, 16, 360);
    assert_eq!(t, vec![thumb(760, 390, 400, 300)]);
}

#[test]
fn the_strip_is_one_centered_row_in_order() {
    let t = strip(&[(400, 300), (800, 300)], SCREEN, 20, 360);
    assert_eq!(
        t,
        vec![thumb(350, 390, 400, 300), thumb(770, 390, 800, 300)]
    );
}

#[test]
fn strip_thumbnails_share_a_height() {
    let t = strip(&[(1600, 1200), (800, 300)], SCREEN, 16, 300);
    assert_eq!(t[0].size, (400, 300));
    assert_eq!(t[0].scale, 0.25);
    assert_eq!(t[1].size, (800, 300));
    assert_eq!(t[1].scale, 1.0);
    assert_eq!(t[1].pos.0, t[0].pos.0 + 400 + 16);
}

#[test]
fn a_long_strip_shrinks_to_fit_the_area() {
    let sizes = vec![(1920, 1080); 10];
    let t = strip(&sizes, SCREEN, 16, 360);
    assert!(t.iter().all(|t| t.scale < 0.5));
    assert!(t[0].pos.0 >= 0);
    let last = t[9];
    assert!(last.pos.0 + last.size.0 <= 1920);
}

#[test]
fn strip_stays_inside_the_area_without_overlap() {
    for n in 1..30usize {
        for width in [2 * n as i32, 200, 1000, 1920] {
            if width < 2 * n as i32 {
                continue;
            }
            let area = (50, 20, width, 600);
            let sizes: Vec<Size> = (0..n as i32)
                .map(|i| (300 + 37 * i, 200 + 53 * (i % 5)))
                .collect();
            let t = strip(&sizes, area, 24, 200);
            assert_eq!(t.len(), n);
            let mut right = area.0;
            for t in &t {
                assert!(t.size.0 >= 1 && t.size.1 >= 1);
                assert!(t.pos.0 >= right, "overlap, n={n} width={width}");
                assert!(t.pos.1 >= area.1 && t.pos.1 + t.size.1 <= area.1 + area.3);
                right = t.pos.0 + t.size.0;
            }
            assert!(right <= area.0 + area.2, "n={n} width={width}");
        }
    }
}

#[test]
fn releasing_a_held_modifier_ends_the_switcher() {
    let alt = (true, false, false);
    assert!(!modifier_released(alt, alt));
    assert!(modifier_released(alt, (false, false, false)));
    // another modifier coming or going doesn't matter
    assert!(!modifier_released(alt, (true, true, true)));
    assert!(modifier_released((true, false, true), (true, false, false)));
    // nothing held at open: nothing to let go of
    assert!(!modifier_released(
        (false, false, false),
        (false, false, false)
    ));
}
