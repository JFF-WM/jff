//! Tests for `jff_wm::primitives::overview_grid`.

use std::time::Duration;

use jff_wm::primitives::geom::Size;
use jff_wm::primitives::layout;
use jff_wm::primitives::layout::Rect;
use jff_wm::primitives::overview_grid::{
    anchor_in, arrange, cycle, dragged_thumb, drop_pos, ease_out_cubic, hit_test, lerp_thumb,
    modifier_released, moved_past, neighbor, progress, shifted, slide_at, strip, strip_offset,
    wall, wall_hit, Dir, Thumb,
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
    assert_eq!(strip_offset(&[], 0, SCREEN), 0);
}

#[test]
fn a_lone_window_in_the_strip_keeps_its_size() {
    let t = strip(&[(400, 300)], SCREEN, 16, 360);
    assert_eq!(t, vec![thumb(0, 390, 400, 300)]);
}

#[test]
fn the_strip_is_one_row_from_the_left_edge_in_order() {
    let t = strip(&[(400, 300), (800, 300)], SCREEN, 20, 360);
    assert_eq!(t, vec![thumb(0, 390, 400, 300), thumb(420, 390, 800, 300)]);
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
fn strip_thumbnails_keep_their_size_however_many_there_are() {
    let one = strip(&[(1920, 1080)], SCREEN, 16, 360);
    let many = strip(&vec![(1920, 1080); 10], SCREEN, 16, 360);
    assert_eq!(one[0].size, (640, 360));
    assert!(many.iter().all(|t| t.size == (640, 360)));
    // The row runs off the area instead of shrinking.
    let last = many[9];
    assert_eq!(last.pos.0, 9 * (640 + 16));
    assert!(last.pos.0 + last.size.0 > SCREEN.2);
}

#[test]
fn a_window_wider_than_the_area_is_scaled_to_fit_it() {
    let t = strip(&[(8000, 1000)], SCREEN, 16, 360);
    assert_eq!(t[0].size, (1920, 240));
    assert_eq!(t[0].scale, 0.24);
}

#[test]
fn a_row_that_fits_is_centered_as_a_whole() {
    let t = strip(&[(400, 300), (800, 300)], SCREEN, 20, 360);
    // row is 0..1220, its middle goes to 960
    assert_eq!(strip_offset(&t, 0, SCREEN), 350);
    assert_eq!(strip_offset(&t, 1, SCREEN), 350);
}

#[test]
fn a_row_that_does_not_fit_slides_to_center_the_selected_one() {
    let t = strip(&vec![(1920, 1080); 10], SCREEN, 16, 360);
    for i in 0..10 {
        let dx = strip_offset(&t, i, SCREEN);
        let t = shifted(t[i], dx);
        assert_eq!(t.pos.0 + t.size.0 / 2, 960, "selected {i}");
    }
    // moving the selection one on moves the row one thumbnail plus gap back
    let step = strip_offset(&t, 4, SCREEN) - strip_offset(&t, 5, SCREEN);
    assert_eq!(step, 640 + 16);
}

#[test]
fn the_offset_is_relative_to_the_area() {
    let t = strip(&vec![(1920, 1080); 10], (100, 0, 1000, 600), 16, 200);
    let dx = strip_offset(&t, 7, (100, 0, 1000, 600));
    let t = shifted(t[7], dx);
    assert_eq!(t.pos.0 + t.size.0 / 2, 600);
}

#[test]
fn a_selection_past_the_end_counts_as_the_first_window() {
    let t = strip(&vec![(1920, 1080); 10], SCREEN, 16, 360);
    assert_eq!(strip_offset(&t, 99, SCREEN), strip_offset(&t, 0, SCREEN));
}

#[test]
fn shifting_moves_only_sideways() {
    let t = shifted(thumb(10, 20, 30, 40), -15);
    assert_eq!(t, thumb(-5, 20, 30, 40));
}

#[test]
fn a_slide_eases_from_where_it_was_to_where_it_goes() {
    let d = Duration::from_millis(120);
    assert_eq!(slide_at(-300.0, 500.0, Duration::ZERO, d), -300.0);
    assert_eq!(slide_at(-300.0, 500.0, d, d), 500.0);
    assert_eq!(slide_at(-300.0, 500.0, d * 3, d), 500.0);
    // not a jump, and fast at the start: more than half way after half the time
    let half = slide_at(0.0, 100.0, d / 2, d);
    assert!(half > 50.0 && half < 100.0, "{half}");
    let mut last = 0.0;
    for ms in 0..=120 {
        let now = slide_at(0.0, 100.0, Duration::from_millis(ms), d);
        assert!(now >= last);
        last = now;
    }
    // sliding back works the same way
    assert!(slide_at(100.0, 0.0, d / 2, d) < 50.0);
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

#[test]
fn wall_is_one_monitor_shaped_square_per_workspace_centered_at_the_bottom() {
    let cells = wall(4, SCREEN, (1920, 1080), 90, 10, 20);
    assert_eq!(cells.len(), 4);
    // 90 px tall, monitor shaped (16:9).
    assert_eq!((cells[0].2, cells[0].3), (160, 90));
    // Same row, 10 px apart, bottom edge 20 px above the area's.
    assert!(cells.iter().all(|c| c.1 == 1080 - 20 - 90));
    assert_eq!(cells[1].0, cells[0].0 + 160 + 10);
    // Centered: as much room left as right.
    let right = SCREEN.2 - (cells[3].0 + cells[3].2);
    assert_eq!(cells[0].0, right);
}

#[test]
fn wall_shrinks_to_fit_and_handles_nothing() {
    assert!(wall(0, SCREEN, (1920, 1080), 90, 10, 20).is_empty());
    let cells = wall(30, SCREEN, (1920, 1080), 200, 10, 20);
    let last = cells.last().unwrap();
    assert!(cells[0].0 >= 0 && last.0 + last.2 <= SCREEN.2);
    assert!(cells[0].3 < 200);
}

#[test]
fn wall_hit_finds_the_square_under_the_point() {
    let cells = wall(3, SCREEN, (1920, 1080), 90, 10, 20);
    let c = cells[1];
    assert_eq!(
        wall_hit(&cells, (c.0 as f64 + 1.0, c.1 as f64 + 1.0)),
        Some(1)
    );
    assert_eq!(wall_hit(&cells, (5.0, 5.0)), None);
}
