//! Surface opacity maths (wp_alpha_modifier_v1).
//!
//! A client's multiplier fades its surface on top of the surface's own pixel alpha. These are
//! the decisions the effects around a surface (blur, border, direct scanout) take from it.

/// Smaller steps than this are invisible on an 8-bit output. Also absorbs the rounding of the
/// protocol's `u32` factor on its way to `f32`, so "fully opaque" really counts as opaque.
const EPSILON: f32 = 1.0 / 1024.0;

/// Whether a surface with this multiplier covers what is behind it (given opaque pixels). Only
/// such a surface may be put straight on the primary plane.
pub fn is_opaque(multiplier: f32) -> bool {
    multiplier >= 1.0 - EPSILON
}

/// Whether a surface with this multiplier cannot be seen at all, so nothing needs drawing for
/// it: no blur, no border.
pub fn is_invisible(multiplier: f32) -> bool {
    multiplier < EPSILON
}

/// `color` (straight RGBA) faded by `multiplier`: only the alpha changes.
pub fn faded(color: [f32; 4], multiplier: f32) -> [f32; 4] {
    [
        color[0],
        color[1],
        color[2],
        color[3] * multiplier.clamp(0.0, 1.0),
    ]
}
