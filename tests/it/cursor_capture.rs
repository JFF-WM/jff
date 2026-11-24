//! Where the cursor is in a capture of a monitor or window.

use jff_wm::primitives::cursor_capture::{hotspot_in_source, pixel_at, pixels};

#[test]
fn a_picture_is_never_cut_short() {
    assert_eq!(pixels(24, 1.0), 24);
    assert_eq!(pixels(24, 1.25), 30);
    assert_eq!(pixels(25, 1.5), 38);
    // 50 * 1.1 is a hair over 55 in floating point: the ceiling must not add a whole pixel
    // for a size that is only an inexact whole number.
    assert_eq!(pixels(50, 1.1), 55);
}

#[test]
fn a_picture_is_at_least_one_pixel() {
    assert_eq!(pixels(0, 2.0), 1);
}

#[test]
fn a_hotspot_is_the_nearest_pixel() {
    assert_eq!(pixel_at(4, 1.5), 6);
    assert_eq!(pixel_at(3, 1.25), 4);
    assert_eq!(pixel_at(-2, 1.0), 0);
}

#[test]
fn the_position_is_in_the_sources_pixels() {
    let at = hotspot_in_source((100.0, 50.0), 2.0, (0, 0), (48, 48), (3840, 2160));
    assert_eq!(at, Some((200, 100)));
}

#[test]
fn a_cursor_off_the_source_is_not_on_it() {
    // Pointer far right of a 1920 wide source.
    assert_eq!(
        hotspot_in_source((3000.0, 10.0), 1.0, (0, 0), (24, 24), (1920, 1080)),
        None
    );
    // And above it.
    assert_eq!(
        hotspot_in_source((10.0, -100.0), 1.0, (0, 0), (24, 24), (1920, 1080)),
        None
    );
}

#[test]
fn a_cursor_partly_on_the_source_is_on_it() {
    // The hotspot is 5 px left of the source, but the picture reaches 19 px into it.
    assert_eq!(
        hotspot_in_source((-5.0, 10.0), 1.0, (0, 0), (24, 24), (1920, 1080)),
        Some((-5, 10))
    );
    // The hotspot is 10 px below the bottom edge, the picture starts 12 px above it.
    assert_eq!(
        hotspot_in_source((10.0, 1090.0), 1.0, (0, 12), (24, 24), (1920, 1080)),
        Some((10, 1090))
    );
}

#[test]
fn a_picture_that_only_touches_the_edge_is_not_on_the_source() {
    // The picture ends exactly where the source starts.
    assert_eq!(
        hotspot_in_source((-24.0, 10.0), 1.0, (0, 0), (24, 24), (1920, 1080)),
        None
    );
}
