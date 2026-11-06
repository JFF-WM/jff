//! Tests for `jff_wm::feature::screenshot::capture`.

use smithay::input::keyboard::Keysym;

use jff_wm::feature::screenshot::capture::{
    around, clip, frame_bars, screenshot_key, ScreenshotKey,
};
use jff_wm::primitives::layout::Rect;

fn area((_, _, w, h): Rect) -> i64 {
    i64::from(w.max(0)) * i64::from(h.max(0))
}

#[test]
fn keys_of_the_selection() {
    assert_eq!(screenshot_key(Keysym::Escape), Some(ScreenshotKey::Cancel));
    assert_eq!(screenshot_key(Keysym::Return), Some(ScreenshotKey::Confirm));
    assert_eq!(
        screenshot_key(Keysym::KP_Enter),
        Some(ScreenshotKey::Confirm)
    );
    assert_eq!(screenshot_key(Keysym::space), Some(ScreenshotKey::Confirm));
    assert_eq!(screenshot_key(Keysym::a), None);
}

#[test]
fn the_dim_areas_cover_everything_but_the_selection() {
    let full = (0, 0, 1920, 1080);
    let hole = (100, 50, 400, 300);
    let parts = around(full, Some(hole));
    let covered: i64 = parts.iter().map(|r| area(*r)).sum();
    assert_eq!(covered, area(full) - area(hole));
    assert_eq!(parts[0], (0, 0, 1920, 50));
    assert_eq!(parts[1], (0, 350, 1920, 730));
    assert_eq!(parts[2], (0, 50, 100, 300));
    assert_eq!(parts[3], (500, 50, 1420, 300));
}

#[test]
fn without_a_selection_everything_is_dimmed() {
    let full = (0, 0, 800, 600);
    let parts = around(full, None);
    assert_eq!(parts[0], full);
    assert!(parts[1..].iter().all(|r| area(*r) == 0));
}

#[test]
fn a_selection_at_the_edge_leaves_empty_parts() {
    let full = (0, 0, 100, 100);
    let parts = around(full, Some(full));
    assert!(parts.iter().all(|r| area(*r) == 0));
}

#[test]
fn the_frame_surrounds_the_selection() {
    let bars = frame_bars((10, 20, 100, 50), 2);
    assert_eq!(bars[0], (8, 18, 104, 2));
    assert_eq!(bars[1], (8, 70, 104, 2));
    assert_eq!(bars[2], (8, 20, 2, 50));
    assert_eq!(bars[3], (110, 20, 2, 50));
}

#[test]
fn clipping_keeps_the_inside() {
    let bounds = (0, 0, 100, 100);
    assert_eq!(clip((10, 10, 20, 20), bounds), Some((10, 10, 20, 20)));
    assert_eq!(clip((-10, -10, 30, 30), bounds), Some((0, 0, 20, 20)));
    assert_eq!(clip((90, 90, 50, 50), bounds), Some((90, 90, 10, 10)));
    assert_eq!(clip((200, 0, 10, 10), bounds), None);
}
