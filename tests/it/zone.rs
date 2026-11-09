//! Tests for `jff_wm::primitives::zone`.

use jff_wm::primitives::zone::{
    bounds_of, center_of, default_origin, delta, offset_in, origin_in, sanitize, shifted,
    to_desktop, to_zone,
};

#[test]
fn zone_and_desktop_coordinates_round_trip() {
    let origin = (1920, 40);
    assert_eq!(to_desktop(origin, (10, 20)), (1930, 60));
    assert_eq!(to_zone(origin, (1930, 60)), (10, 20));
    assert_eq!(to_zone(origin, to_desktop(origin, (-5, 7))), (-5, 7));
}

#[test]
fn a_request_inside_the_zone_is_kept() {
    assert_eq!(sanitize((100, 50), (300, 200), (1920, 1080)), (100, 50));
}

#[test]
fn a_request_outside_the_zone_is_pulled_in() {
    let zone = (1000, 800);
    assert_eq!(sanitize((-30, -1), (300, 200), zone), (0, 0));
    assert_eq!(sanitize((5000, 5000), (300, 200), zone), (700, 600));
    // Bigger than the zone: it starts at the corner.
    assert_eq!(sanitize((50, 50), (2000, 2000), zone), (0, 0));
}

#[test]
fn an_infinite_side_is_not_bounded() {
    assert_eq!(sanitize((5000, 10), (300, 200), (0, 800)), (5000, 10));
    assert_eq!(sanitize((-5, 10), (300, 200), (0, 0)), (0, 10));
}

#[test]
fn moving_a_zone_moves_positions_by_the_same_delta() {
    let d = delta((100, 100), (130, 90));
    assert_eq!(d, (30, -10));
    assert_eq!(shifted((200, 200), d), (230, 190));
}

#[test]
fn the_middle_of_a_group_of_windows_is_the_middle_of_their_box() {
    assert_eq!(center_of(&[]), None);
    assert_eq!(center_of(&[((0, 0), (100, 100))]), Some((50.0, 50.0)));
    assert_eq!(
        center_of(&[((0, 0), (100, 100)), ((300, 100), (100, 100))]),
        Some((200.0, 100.0))
    );
}

#[test]
fn a_zone_keeps_its_place_on_a_monitor_that_moves() {
    let before = (0, 0, 1920, 1080);
    let after = (1280, 0, 1920, 1080);
    let offset = offset_in(before, (300, 120));
    assert_eq!(origin_in(after, offset), (1580, 120));
    assert_eq!(default_origin((0, 32, 1920, 1048)), (0, 32));
}

#[test]
fn the_box_around_windows_covers_all_of_them() {
    assert_eq!(bounds_of(&[]), None);
    assert_eq!(
        bounds_of(&[((10, 10), (100, 50)), ((200, 0), (50, 50))]),
        Some(((10, 0), (240, 60)))
    );
}
