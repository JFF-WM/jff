//! Which surfaces declared an alpha mode other than premultiplied electrical
//! (wp-color-representation): straight, or premultiplied optical. The blending wants
//! premultiplied electrical, so the shader of `rounded` converts these, see
//! [`Rounding::with_alpha_mode`](super::rounded::Rounding::with_alpha_mode).

use smithay::backend::renderer::element::Id;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};

use crate::protocol::color_representation::{color_representation_in, ever_used, AlphaMode};

/// The surfaces of the tree of `root` that declared an alpha mode the plain draw gets wrong,
/// by the id their render element has. Empty (and cheap) for a window that never used the
/// protocol.
pub fn modes(root: &WlSurface) -> Vec<(Id, AlphaMode)> {
    if !ever_used() {
        return Vec::new();
    }
    let mut found = Vec::new();
    with_surface_tree_downward(
        root,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |surface, states, _| {
            let mode = color_representation_in(states).alpha_mode;
            if mode != AlphaMode::default() {
                found.push((Id::from_wayland_resource(surface), mode));
            }
        },
        |_, _, _| true,
    );
    found
}

/// The mode of the element `id` among `found`, premultiplied electrical when it is not there.
pub fn find(found: &[(Id, AlphaMode)], id: &Id) -> AlphaMode {
    found
        .iter()
        .find(|(candidate, _)| candidate == id)
        .map_or_else(AlphaMode::default, |(_, mode)| *mode)
}
