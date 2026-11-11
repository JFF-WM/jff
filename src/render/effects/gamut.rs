//! Colors of surfaces (wp-color-management, stage 2): which surfaces need their colors converted
//! to the monitor's, and how. The conversion itself runs in `rounded.frag`, see
//! [`Rounding::with_conversion`](super::rounded::Rounding::with_conversion).
//!
//! Almost everything jff draws is a tree of Wayland surfaces: windows, their popups, layer
//! surfaces and their menus, the lock screen, input method candidates, overview thumbnails and
//! the closing picture of a window. Each of those goes through [`wrap_surfaces`] (or, for a
//! thumbnail that is scaled and moved afterwards, [`tree_conversions`] and [`masked`]), which
//! decides per surface from what the *frame in progress* is converted to.
//!
//! That target is not passed down every function that builds elements: the frame builder says
//! it once with [`enter`] and the guard it returns keeps it for the rest of the builder. It is
//! thread-local like the other per-frame caches in `rounded.rs`, and a builder that does not
//! enter a scope draws everything unconverted, exactly as before.

use std::cell::Cell;

use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};

use crate::primitives::color::{ImageDescription, RenderIntent};
use crate::primitives::gamut::Conversion;
use crate::protocol::color_management::{surface_color_in, OutputColor};
use crate::render::effects::rounded::Rounding;
use crate::render::elements::JffElement;

thread_local! {
    /// What the frame being built is converted to. `None`: nothing is converted.
    static ACTIVE: Cell<Option<OutputColor>> = const { Cell::new(None) };
}

/// Keeps a target active until it is dropped, then puts the previous one back.
pub struct Scope(Option<OutputColor>);

impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(self.0));
    }
}

/// Convert every surface drawn from here on (until the guard is dropped) to `color`. `None`
/// draws them unconverted.
#[must_use = "the target is only active while the guard is alive"]
pub fn enter(color: Option<OutputColor>) -> Scope {
    Scope(ACTIVE.with(|active| active.replace(color)))
}

/// What the frame being built is converted to.
pub fn active() -> Option<OutputColor> {
    ACTIVE.with(Cell::get)
}

/// The surfaces of the tree of `root` that are not shown as they are, with the conversion each
/// needs, by the id their render element has. Surfaces with no description are plain sRGB: they
/// are in it only with `convert_srgb`, otherwise they are drawn as they always were. Empty (and
/// cheap) for a window that never used the protocol, on a monitor that is sRGB.
pub fn conversions(root: &WlSurface, color: &OutputColor) -> Vec<(Id, Conversion)> {
    let mut found = Vec::new();
    with_surface_tree_downward(
        root,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |surface, states, _| {
            let (description, intent) = match surface_color_in(states) {
                Some(color) => (color.description, color.intent),
                None => (ImageDescription::srgb(), RenderIntent::Perceptual),
            };
            let conversion =
                Conversion::to_output(&description, &color.target, intent, color.convert_srgb);
            if let Some(conversion) = conversion {
                found.push((Id::from_wayland_resource(surface), conversion));
            }
        },
        |_, _, _| true,
    );
    found
}

/// [`conversions`] for the frame in progress, empty when it has no target.
pub fn tree_conversions(root: &WlSurface) -> Vec<(Id, Conversion)> {
    active().map_or_else(Vec::new, |color| conversions(root, &color))
}

/// The conversion for the element `id` among `found`.
pub fn find(found: &[(Id, Conversion)], id: &Id) -> Option<Conversion> {
    found
        .iter()
        .find(|(candidate, _)| candidate == id)
        .map(|(_, conversion)| *conversion)
}

/// `base` (the rounding of a window, if it has one) with `conversion` added, a pass-through mask
/// that only converts when there is no `base`. `None` when there is nothing to draw through the
/// shader, or the shader is unavailable and so the surface is drawn as it is.
pub fn masked(
    renderer: &mut GlesRenderer,
    base: Option<&Rounding>,
    conversion: Option<Conversion>,
) -> Option<Rounding> {
    match (base, conversion) {
        (Some(base), Some(conversion)) => Some(base.clone().with_conversion(conversion)),
        (Some(base), None) => Some(base.clone()),
        (None, Some(conversion)) => {
            Rounding::passthrough(renderer).map(|mask| mask.with_conversion(conversion))
        }
        (None, None) => None,
    }
}

/// Push the elements of a surface tree rooted at `root`, converting the surfaces that need it
/// to what the frame in progress shows. Surfaces that need nothing, or all of them with no
/// target, are pushed as they are.
pub fn wrap_surfaces(
    renderer: &mut GlesRenderer,
    root: &WlSurface,
    elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
    out: &mut Vec<JffElement>,
) {
    let found = tree_conversions(root);
    if found.is_empty() {
        out.extend(elements.into_iter().map(JffElement::from));
        return;
    }
    for element in elements {
        let conversion = find(&found, element.id());
        match masked(renderer, None, conversion) {
            Some(mask) => out.push(mask.wrap(element)),
            None => out.push(JffElement::from(element)),
        }
    }
}
