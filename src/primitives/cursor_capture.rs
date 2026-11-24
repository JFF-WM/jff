//! The geometry of a cursor capture session (`ext-image-copy-capture-v1`,
//! `create_pointer_cursor_session`): how big the cursor's picture is on a monitor of a given
//! scale, and where it is in the picture of the source. All pure, the compositor side is in
//! `protocol/image_copy.rs`.

/// Pixels a length of `logical` logical pixels takes at `scale`, rounded up so the picture never
/// cuts the last row or column of the cursor. At least 1.
pub fn pixels(logical: i32, scale: f64) -> i32 {
    // 50 * 1.1 is 55.00000000000001: without the margin a whole number of pixels that is only
    // inexact in floating point would grow by one.
    ((f64::from(logical) * scale - 1e-6).ceil() as i32).max(1)
}

/// The same for a position inside the picture (a hotspot): nearest pixel, never negative.
pub fn pixel_at(logical: i32, scale: f64) -> i32 {
    ((f64::from(logical) * scale).round() as i32).max(0)
}

/// Where the cursor's hotspot is in the picture of its source, in the source's pixels, if any of
/// the cursor's picture is inside the source (the protocol's `enter` and `leave`: the cursor is
/// on the source while its image touches it, not only its hotspot).
///
/// - `relative`: the pointer in logical pixels, from the source's top-left corner
/// - `scale`: pixels per logical pixel
/// - `hotspot`: the pixel of the cursor picture that sits on the pointer
/// - `image`: the pixel size of the cursor picture
/// - `source`: the pixel size of the source
pub fn hotspot_in_source(
    relative: (f64, f64),
    scale: f64,
    hotspot: (i32, i32),
    image: (i32, i32),
    source: (i32, i32),
) -> Option<(i32, i32)> {
    let at = (
        (relative.0 * scale).round() as i32,
        (relative.1 * scale).round() as i32,
    );
    // The picture's top-left corner and its far edge.
    let (left, top) = (at.0 - hotspot.0, at.1 - hotspot.1);
    let (right, bottom) = (left + image.0, top + image.1);
    let touches = right > 0 && bottom > 0 && left < source.0 && top < source.1;
    touches.then_some(at)
}
