//! Tests for `jff_wm::feature::overview_mode`.

use smithay::input::keyboard::Keysym;

use jff_wm::feature::overview_mode::{overview_key, OverviewKey};
use jff_wm::primitives::overview_grid::Dir;

#[test]
fn overview_keys() {
    assert_eq!(overview_key(Keysym::Escape), Some(OverviewKey::Cancel));
    assert_eq!(overview_key(Keysym::Return), Some(OverviewKey::Select));
    assert_eq!(
        overview_key(Keysym::ISO_Left_Tab),
        Some(OverviewKey::Cycle { backwards: true })
    );
    assert_eq!(
        overview_key(Keysym::Left),
        Some(OverviewKey::Move(Dir::Left))
    );
    assert_eq!(overview_key(Keysym::a), None);
}
