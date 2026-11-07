//! Tests for `jff_wm::primitives::alpha`.

use jff_wm::primitives::alpha::{faded, is_invisible, is_opaque};

#[test]
fn only_a_full_multiplier_is_opaque() {
    assert!(is_opaque(1.0));
    assert!(!is_opaque(0.5));
    assert!(!is_opaque(0.0));
}

#[test]
fn rounding_of_the_u32_factor_still_counts_as_opaque() {
    // 0xFFFFFFFF as f32 / u32::MAX as f32 may land a hair under 1.0
    assert!(is_opaque(1.0 - f32::EPSILON));
}

#[test]
fn a_barely_visible_surface_is_not_opaque() {
    assert!(!is_opaque(0.99));
}

#[test]
fn only_a_zero_multiplier_is_invisible() {
    assert!(is_invisible(0.0));
    assert!(is_invisible(1.0e-6));
    assert!(!is_invisible(0.01));
    assert!(!is_invisible(1.0));
}

#[test]
fn fading_a_colour_changes_only_its_alpha() {
    assert_eq!(faded([0.1, 0.2, 0.3, 0.8], 0.5), [0.1, 0.2, 0.3, 0.4]);
    assert_eq!(faded([0.1, 0.2, 0.3, 0.8], 1.0), [0.1, 0.2, 0.3, 0.8]);
    assert_eq!(faded([0.1, 0.2, 0.3, 0.8], 0.0), [0.1, 0.2, 0.3, 0.0]);
}

#[test]
fn fading_clamps_a_multiplier_out_of_range() {
    assert_eq!(faded([0.0, 0.0, 0.0, 1.0], 2.0)[3], 1.0);
    assert_eq!(faded([0.0, 0.0, 0.0, 1.0], -1.0)[3], 0.0);
}
