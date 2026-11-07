//! Tests for `jff_wm::input::grabs::r#move`.

use jff_wm::input::grabs::r#move::{dragged_pos, shrunk_geometry};

#[test]
fn window_follows_pointer_delta() {
    let p = dragged_pos((40, 40), (100.0, 100.0).into(), (130.4, 85.0).into());
    assert_eq!(p, (70, 25));
}

#[test]
fn no_movement_keeps_position() {
    assert_eq!(
        dragged_pos((7, 9), (5.5, 5.5).into(), (5.5, 5.5).into()),
        (7, 9)
    );
}

#[test]
fn shrunk_window_is_half_the_output() {
    let (_, size) = shrunk_geometry((0, 0, 1920, 1080), (960.0, 540.0).into());
    assert_eq!(size, (960, 540));
}

#[test]
fn shrunk_window_keeps_pointer_at_same_relative_spot() {
    // Pointer at the middle of the output: the middle of the smaller window.
    let (pos, _) = shrunk_geometry((0, 0, 1920, 1080), (960.0, 540.0).into());
    assert_eq!(pos, (480, 270));
    // Pointer in the top-left corner: the window's top-left corner.
    let (pos, _) = shrunk_geometry((0, 0, 1920, 1080), (0.0, 0.0).into());
    assert_eq!(pos, (0, 0));
    // Bottom-right corner: the window's bottom-right corner.
    let (pos, size) = shrunk_geometry((0, 0, 1920, 1080), (1920.0, 1080.0).into());
    assert_eq!(pos, (1920 - size.0, 1080 - size.1));
}

#[test]
fn shrunk_window_on_second_monitor() {
    let (pos, size) = shrunk_geometry((1920, 0, 1280, 720), (1920.0 + 640.0, 360.0).into());
    assert_eq!(size, (640, 360));
    assert_eq!(pos, (1920 + 320, 180));
}
