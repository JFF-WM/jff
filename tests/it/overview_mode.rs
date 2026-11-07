//! Tests for `jff_wm::feature::overview_mode`.

use smithay::input::keyboard::Keysym;

use jff_wm::feature::overview_mode::{overview_key, typed_char, OverviewKey};
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

#[test]
fn backspace_is_a_key_and_letters_are_not() {
    assert_eq!(
        overview_key(Keysym::BackSpace),
        Some(OverviewKey::Backspace)
    );
    // letters are not keys of their own, they are typed
    assert_eq!(overview_key(Keysym::f), None);
}

#[test]
fn typing_gives_the_character_the_layout_makes() {
    assert_eq!(typed_char(Keysym::f, false, false, false), Some('f'));
    assert_eq!(typed_char(Keysym::F, false, false, false), Some('F'));
    assert_eq!(typed_char(Keysym::space, false, false, false), Some(' '));
    assert_eq!(typed_char(Keysym::ccedilla, false, false, false), Some('ç'));
}

#[test]
fn shortcuts_and_non_text_keys_type_nothing() {
    assert_eq!(typed_char(Keysym::f, true, false, false), None);
    assert_eq!(typed_char(Keysym::f, false, true, false), None);
    assert_eq!(typed_char(Keysym::f, false, false, true), None);
    assert_eq!(typed_char(Keysym::Escape, false, false, false), None);
    assert_eq!(typed_char(Keysym::F5, false, false, false), None);
    assert_eq!(typed_char(Keysym::Shift_L, false, false, false), None);
}
