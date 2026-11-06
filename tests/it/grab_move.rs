//! Tests for `jff_wm::input::grabs::r#move`.

use jff_wm::input::grabs::r#move::dragged_pos;

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
