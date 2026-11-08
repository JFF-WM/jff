//! Colors of YUV video (wp-color-representation, stage 2): which surfaces of a window need
//! their colors fixed, and by how much. The fix itself runs in `rounded.frag`, see
//! [`Rounding::with_correction`](super::rounded::Rounding::with_correction).

use smithay::backend::renderer::element::Id;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};

use crate::primitives::yuv::{Correction, Encoding};
use crate::protocol::color_representation::{
    color_representation_in, Coefficients, Range, DRIVER_COEFFICIENTS,
};

fn encoding((coefficients, range): (Coefficients, Range)) -> Option<Encoding> {
    let (kr, kb) = coefficients.luma_weights()?;
    Some(Encoding {
        kr,
        kb,
        limited: range == Range::Limited,
    })
}

/// The correction a client's declared matrix and range need, `None` when the driver's own
/// conversion is already that (or the pair is not a YCbCr one).
pub fn correction_for(declared: (Coefficients, Range)) -> Option<Correction> {
    Correction::between(encoding(DRIVER_COEFFICIENTS)?, encoding(declared)?)
}

/// The surfaces of the tree of `root` that declared a YUV matrix and range the driver does not
/// use, with the correction each needs, by the id their render element has. Empty (and cheap)
/// for a window that never used the protocol.
pub fn corrections(root: &WlSurface) -> Vec<(Id, Correction)> {
    let mut found = Vec::new();
    with_surface_tree_downward(
        root,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |surface, states, _| {
            let declared = color_representation_in(states).coefficients;
            if let Some(fix) = declared.and_then(correction_for) {
                found.push((Id::from_wayland_resource(surface), fix));
            }
        },
        |_, _, _| true,
    );
    found
}
