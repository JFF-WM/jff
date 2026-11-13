//! Colors of surfaces (wp-color-management): which surfaces need their colors converted to the
//! monitor's, and how. The conversion itself runs in `rounded.frag`, see
//! [`Rounding::with_conversion`](super::rounded::Rounding::with_conversion).
//!
//! On an HDR monitor the target is its working space: every surface is converted into it, and
//! [`working`], [`tint`] and [`framebuffer_format`] tell what else drawn into that frame (borders,
//! blur) has to match.
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
//!
//! An HDR frame also needs to know how bright the content on screen says it gets (`max_cll` or
//! the mastering luminance of a PQ surface), to fit its tone mapping to it. The frame builder
//! asks for that with [`collect_content_light`], the same way: the code that draws real
//! surfaces ([`wrap_surfaces`], and a window's own surface in `background_effect.rs`) notes their
//! content light with [`note_content_light`], `rounded.rs` ties it to the id the surface is
//! finally drawn as ([`bind_content_light`]), and [`take_content_light`] hands the result to
//! the HDR frame, which keeps those the damage tracker really drew. Overview thumbnails and a
//! window that is still opening go through the thumbnail path and are never noted, a closing
//! window is only a picture and has no surface.

use std::cell::{Cell, RefCell};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};

use crate::primitives::color::{ContentLight, ImageDescription, RenderIntent};
use crate::primitives::gamut::Conversion;
use crate::protocol::color_management::{surface_color_in, OutputColor, WorkingSpace};
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

thread_local! {
    /// The frame being drawn right now is the working-space texture of an HDR output.
    static DRAWING_HDR: Cell<bool> = const { Cell::new(false) };
}

/// Marks the HDR frame as being drawn until it is dropped.
pub struct HdrDraw(bool);

impl Drop for HdrDraw {
    fn drop(&mut self) {
        DRAWING_HDR.with(|drawing| drawing.set(self.0));
    }
}

/// From here until the guard is dropped, what is drawn goes to an HDR output's working space.
#[must_use = "the frame is only marked while the guard is alive"]
pub fn begin_hdr_draw() -> HdrDraw {
    HdrDraw(DRAWING_HDR.with(|drawing| drawing.replace(true)))
}

/// The content light of the surfaces of the HDR frame being built.
#[derive(Default)]
struct Lights {
    /// By the id of the surface's own render element, noted when its tree was walked.
    surfaces: Vec<(Id, ContentLight)>,
    /// By the id of the element the surface was drawn as, which is what the damage tracker reports on.
    drawn: Vec<(Id, ContentLight)>,
}

thread_local! {
    /// What the frame being built collects. `None`: nothing is.
    static LIGHTS: RefCell<Option<Lights>> = const { RefCell::new(None) };
}

/// Keeps the collecting of content light going until it is dropped, then puts the previous state back.
pub struct LightScope(Option<Lights>);

impl Drop for LightScope {
    fn drop(&mut self) {
        LIGHTS.with(|lights| *lights.borrow_mut() = self.0.take());
    }
}

/// Collect the content light of the surfaces drawn from here on (until the guard is dropped) when `on`, and nothing when not.
#[must_use = "the content light is only collected while the guard is alive"]
pub fn collect_content_light(on: bool) -> LightScope {
    LightScope(LIGHTS.with(|lights| lights.replace(on.then(Lights::default))))
}

/// The content light of the surfaces drawn so far, by the id of the element each was drawn as, and forgets them. Empty when nothing collects.
pub fn take_content_light() -> Vec<(Id, ContentLight)> {
    LIGHTS.with(|lights| {
        lights
            .borrow_mut()
            .as_mut()
            .map(|lights| std::mem::take(&mut lights.drawn))
            .unwrap_or_default()
    })
}

/// Note how bright the content of each surface of the tree of `root` says it gets, for the HDR frame being built. The surfaces still have to be bound to the element they are drawn as ([`bind_content_light`]) before they count. Does nothing when no frame collects or the frame is not HDR, surfaces that are SDR are not noted.
pub fn note_content_light(root: &WlSurface) {
    if working().is_none() || LIGHTS.with(|lights| lights.borrow().is_none()) {
        return;
    }
    let mut found = Vec::new();
    with_surface_tree_downward(
        root,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |surface, states, _| {
            let light = surface_color_in(states)
                .map_or(ContentLight::Sdr, |color| color.description.content_light());
            if light != ContentLight::Sdr {
                found.push((Id::from_wayland_resource(surface), light));
            }
        },
        |_, _, _| true,
    );
    LIGHTS.with(|lights| {
        if let Some(lights) = lights.borrow_mut().as_mut() {
            lights.surfaces.extend(found);
        }
    });
}

/// The surface element `inner` is drawn as `element` (see `Rounding`): if its content light was noted, it now counts under `element`'s id.
pub fn bind_content_light(inner: &Id, element: &Id) {
    LIGHTS.with(|lights| {
        if let Some(lights) = lights.borrow_mut().as_mut() {
            if let Some(&(_, light)) = lights.surfaces.iter().find(|(id, _)| id == inner) {
                lights.drawn.push((element.clone(), light));
            }
        }
    });
}

/// Format of a copy of what is on the framebuffer being drawn: float for the working space, 8 bits otherwise.
pub fn framebuffer_format() -> Fourcc {
    if DRAWING_HDR.with(Cell::get) {
        super::hdr::WORKING_FORMAT
    } else {
        Fourcc::Abgr8888
    }
}

/// What the frame being built is converted to.
pub fn active() -> Option<OutputColor> {
    ACTIVE.with(Cell::get)
}

/// The working space the frame being built is composited in, `None` for an SDR frame.
pub fn working() -> Option<WorkingSpace> {
    active().and_then(|color| color.hdr)
}

/// A color given in sRGB (straight alpha in the fourth value) as the frame being built has it: unchanged for an SDR frame, linear light in the working space for an HDR one.
pub fn tint(color: [f32; 4]) -> [f32; 4] {
    match working() {
        Some(space) => {
            let [r, g, b] = srgb_to_working(space, [color[0], color[1], color[2]]);
            [r, g, b, color[3]]
        }
        None => color,
    }
}

/// Straight sRGB values as the working space has them.
pub fn srgb_to_working(space: WorkingSpace, rgb: [f32; 3]) -> [f32; 3] {
    Conversion::to_working(&ImageDescription::srgb(), space.sdr_white as f32).apply(rgb)
}

/// The conversion each surface of the tree of `root` needs, by its element id: every surface for an HDR frame, else those not shown as they are (plain sRGB ones only with `convert_srgb`).
pub fn conversions(root: &WlSurface, color: &OutputColor) -> Vec<(Id, Conversion)> {
    let mut found = Vec::new();
    // An HDR frame has no surface that is drawn as it is: all of them are brought into the working space.
    if let Some(space) = color.hdr {
        with_surface_tree_downward(
            root,
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |surface, states, _| {
                let description = surface_color_in(states)
                    .map_or_else(ImageDescription::srgb, |color| color.description);
                found.push((
                    Id::from_wayland_resource(surface),
                    Conversion::to_working(&description, space.sdr_white as f32),
                ));
            },
            |_, _, _| true,
        );
        return found;
    }
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
    note_content_light(root);
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
