//! Blur placement for surfaces, recomputed every frame.
//!
//! A surface gets a blurred background when its client sent a non-empty blur region
//! (`ext_background_effect_v1`, see `protocol/background_effect.rs`) and blur is enabled in
//! config. The blur covers exactly that region. Each such surface keeps one
//! [`BackgroundEffect`] in its user data, so element identity and the change counter live as
//! long as the surface.

use std::sync::{Arc, Mutex};

use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::utils::{
    Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::{AsRenderElements, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
use smithay::desktop::{PopupManager, Window};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};
use smithay::wayland::compositor::{with_states, SurfaceData};

use super::blur::BlurOptions;
use super::border::{border_element, BorderPaint};
use super::framebuffer_effect::{FramebufferEffect, FramebufferEffectElement, WallpaperSnapshot};
use super::rounded::{is_fullscreen, Rounding};
use crate::config;
use crate::feature::window_anim::OpenFrame;
use crate::primitives::region::TransformedRegion;
use crate::protocol::background_effect::get_cached_blur_region;
use crate::render::elements::JffElement;

#[derive(Debug)]
pub struct RenderParams {
    /// Visible surface rectangle, in frame coordinates.
    pub geometry: Rectangle<f64, Logical>,
    /// Blurred parts of `geometry`, in the same coordinates with the region offset applied.
    pub subregion: TransformedRegion,
    /// Rounded corners of the window this blur sits behind: the blur is painted through the
    /// same mask as the window, so it doesn't show square around round corners.
    pub mask: Option<Rounding>,
}

#[derive(Debug)]
struct BackgroundEffect {
    framebuffer: FramebufferEffect,
    options: Option<BlurOptions>,
}

impl BackgroundEffect {
    fn damage(&mut self) {
        self.framebuffer.damage();
    }

    fn set_options(&mut self, options: BlurOptions) {
        if self.options != Some(options) {
            self.options = Some(options);
            self.damage();
        }
    }
}

struct SurfaceBackgroundEffect(Mutex<BackgroundEffect>);

impl SurfaceBackgroundEffect {
    fn get(states: &SurfaceData) -> &Self {
        states.data_map.get_or_insert(|| {
            SurfaceBackgroundEffect(Mutex::new(BackgroundEffect {
                framebuffer: FramebufferEffect::new(),
                options: None,
            }))
        })
    }
}

/// Repaint the blur of the surface owning `states`. Called when its blur region changes.
pub fn damage_surface(states: &SurfaceData) {
    if let Some(effect) = states.data_map.get::<SurfaceBackgroundEffect>() {
        effect.0.lock().unwrap().damage();
    }
}

fn surface_view_geo(states: &SurfaceData) -> Option<Rectangle<i32, Logical>> {
    let data = states.data_map.get::<RendererSurfaceStateUserData>()?;
    let view = data.lock().unwrap().view()?;
    Some(Rectangle::new(view.offset, view.dst))
}

/// Push the blur element for `surface`, if it should have one.
///
/// * Non-empty client region: blur exactly that region.
/// * Empty client region: no blur.
/// * No region and `fallback_geo` set: blur that rectangle (only passed when
///   `all-windows` / `all-layers` asks for it).
///
/// The element goes *after* the surface's own elements in the front-to-back list: the list is
/// drawn from the end and the blur samples the frame at draw time, so it must be drawn right
/// before the surface, directly behind it.
pub fn render_for_surface(
    surface: &WlSurface,
    surface_loc: Point<f64, Logical>,
    fallback_geo: Option<Rectangle<i32, Logical>>,
    scale: f64,
    blur: config::Blur,
    push: &mut dyn FnMut(FramebufferEffectElement),
) {
    render_for_surface_zoomed(
        surface,
        surface_loc,
        1.0,
        fallback_geo,
        scale,
        blur,
        None,
        push,
    );
}

/// [`render_for_surface`] for a surface drawn shrunk (overview thumbnail).
///
/// `zoom` scales the surface around its own origin (1.0 = real size), `surface_loc` is where
/// that origin lands in the frame. Blur area and the client region are scaled by `zoom` too,
/// so the blur covers the shrunk window. `mask`: the window's rounded corners (made for the
/// same zoom), the blur is shaped by it.
pub fn render_for_surface_zoomed(
    surface: &WlSurface,
    surface_loc: Point<f64, Logical>,
    zoom: f64,
    fallback_geo: Option<Rectangle<i32, Logical>>,
    scale: f64,
    blur: config::Blur,
    mask: Option<&Rounding>,
    push: &mut dyn FnMut(FramebufferEffectElement),
) {
    if blur.off {
        return;
    }
    let options = BlurOptions::from(blur);

    with_states(surface, |states| {
        // Read the region before locking the effect: the commit hook takes the locks in the
        // opposite order.
        let client_region = get_cached_blur_region(states);

        let Some(view) = surface_view_geo(states) else {
            return;
        };

        let (rects, area) = match client_region {
            Some(rects) if rects.is_empty() => return,
            Some(rects) => {
                let view = view.to_f64();
                let area = Rectangle::new(
                    view.loc.upscale(Scale::from(zoom)) + surface_loc,
                    view.size.upscale(Scale::from(zoom)),
                );
                (rects, area)
            }
            None => {
                let Some(geo) = fallback_geo else {
                    return;
                };
                let geo_f = geo.to_f64();
                let area = Rectangle::new(
                    geo_f.loc.upscale(Scale::from(zoom)) + surface_loc,
                    geo_f.size.upscale(Scale::from(zoom)),
                );
                (Arc::new(vec![Rectangle::from_size(geo.size)]), area)
            }
        };

        let effect = SurfaceBackgroundEffect::get(states);
        let mut effect = effect.0.lock().unwrap();
        effect.set_options(options);

        let subregion = TransformedRegion {
            rects,
            // Region rectangles are in the client's unscaled coordinates.
            scale: Scale::from(zoom),
            offset: area.loc,
        };
        let geometry = area.to_physical_precise_round(scale).to_logical(scale);

        push(effect.framebuffer.render(
            RenderParams {
                geometry,
                subregion,
                mask: mask.cloned(),
            },
            options,
        ));
    });
}

/// Elements of the popups of `parent`, each followed in the list (= drawn right behind it) by
/// its blur.
///
/// `parent_loc` is the parent surface's origin in the frame, `parent_geo_loc` is its geometry
/// offset inside the surface (popups are placed relative to parent geometry, layer surfaces
/// pass zero). A popup blurs the live frame, so what sits behind it is the parent window, not
/// the wallpaper. `blur_whole`: also blur popups that sent no region. `radius`: corner radius
/// of the popups (0 = square), their blur is shaped by it so it doesn't show square behind a
/// rounded popover. The mask is the popup's geometry, a client shadow around it is not blurred
/// differently.
pub fn popup_elements(
    renderer: &mut GlesRenderer,
    parent: &WlSurface,
    parent_loc: Point<i32, Logical>,
    parent_geo_loc: Point<i32, Logical>,
    blur: Option<config::Blur>,
    blur_whole: bool,
    radius: i32,
    out: &mut Vec<JffElement>,
) {
    let scale = Scale::from(1.0f64);
    for (popup, popup_offset) in PopupManager::popups_for_surface(parent) {
        let origin = parent_loc + parent_geo_loc + popup_offset - popup.geometry().loc;
        let popup_geo = Rectangle::new(origin + popup.geometry().loc, popup.geometry().size);
        let loc: Point<i32, Physical> = origin.to_physical_precise_round(scale);
        let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                &mut *renderer,
                popup.wl_surface(),
                loc,
                scale,
                1.0,
                Kind::Unspecified,
            );
        out.extend(elements.into_iter().map(JffElement::from));
        if let Some(blur) = blur {
            let mask = Rounding::new(&mut *renderer, popup_geo, radius);
            render_for_surface_zoomed(
                popup.wl_surface(),
                origin.to_f64(),
                1.0,
                blur_whole.then_some(popup.geometry()),
                1.0,
                blur,
                mask.as_ref(),
                &mut |element| out.push(JffElement::from(element)),
            );
        }
    }
}

/// Elements of a window: its popups, then the window itself, each followed (= drawn right
/// behind it) by its blur.
///
/// `surface_loc` is the window surface origin in the frame. `snapshot`: the window blurs a
/// wallpaper copy instead of the live frame (see `WallpaperSnapshot`). `radius` is the corner
/// radius in pixels (0 = square), fullscreen windows are never rounded. `popup_radius` is the
/// same for the blur behind the window's popups.
/// `border` is the outline around the window (see `border`), `None` for none. `alpha` is the
/// look of the window while it opens (`OpenFrame::DONE` once it has).
pub fn window_elements(
    renderer: &mut GlesRenderer,
    window: &Window,
    surface_loc: Point<i32, Logical>,
    blur: Option<config::Blur>,
    snapshot: Option<&WallpaperSnapshot>,
    radius: i32,
    popup_radius: i32,
    border: Option<BorderPaint>,
    open: OpenFrame,
    out: &mut Vec<JffElement>,
) {
    let alpha = open.alpha;
    // A window that is still opening has no blur and no outline: both would show at full
    // strength behind a half-transparent or moving window.
    let (blur, border) = if open.settled() {
        (blur, border)
    } else {
        (None, None)
    };
    let scale = Scale::from(1.0f64);
    let loc: Point<i32, Physical> = surface_loc.to_physical_precise_round(scale);
    let geo = window.geometry();
    if open.scale != 1.0 || open.dy != 0 {
        scaled_window(renderer, window, surface_loc, radius, open, out);
        return;
    }
    let toplevel = window.toplevel();

    // A fullscreen window covers the monitor with square corners, and stays eligible for
    // direct scanout.
    let rounding = if is_fullscreen(window) {
        None
    } else {
        Rounding::new(
            renderer,
            Rectangle::new(surface_loc + geo.loc, geo.size),
            radius,
        )
    };

    // The outline goes last in the list: drawn behind the window and its blur.
    let ring = border_element(
        renderer,
        window,
        Rectangle::new(surface_loc + geo.loc, geo.size),
        radius,
        border,
    );

    // Without blur and rounding (or for an X11 window) there's nothing to interleave.
    let (Some(toplevel), true) = (toplevel, blur.is_some() || rounding.is_some()) else {
        let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            window.render_elements(&mut *renderer, loc, scale, alpha);
        out.extend(surfaces.into_iter().map(JffElement::from));
        out.extend(ring.map(JffElement::Border));
        return;
    };
    popup_elements(
        renderer,
        toplevel.wl_surface(),
        surface_loc,
        geo.loc,
        blur,
        blur.is_some_and(|b| b.all_windows),
        popup_radius,
        out,
    );
    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        render_elements_from_surface_tree(
            &mut *renderer,
            toplevel.wl_surface(),
            loc,
            scale,
            alpha,
            Kind::Unspecified,
        );
    // Popups above stay square: only the window's own surfaces go through the mask.
    out.extend(surfaces.into_iter().map(|element| match &rounding {
        Some(rounding) => rounding.wrap(element),
        None => JffElement::from(element),
    }));
    if let Some(blur) = blur {
        render_for_surface_zoomed(
            toplevel.wl_surface(),
            surface_loc.to_f64(),
            1.0,
            blur.all_windows.then_some(geo),
            1.0,
            blur,
            rounding.as_ref(),
            &mut |element| out.push(JffElement::from(element.with_wallpaper_source(snapshot))),
        );
    }
    out.extend(ring.map(JffElement::Border));
}

/// A window that is growing or sliding in, drawn like an overview thumbnail: at the origin,
/// scaled around its geometry's top-left, then moved so it stays centered. No popups.
fn scaled_window(
    renderer: &mut GlesRenderer,
    window: &Window,
    surface_loc: Point<i32, Logical>,
    radius: i32,
    open: OpenFrame,
    out: &mut Vec<JffElement>,
) {
    let scale = Scale::from(1.0f64);
    let geo = window.geometry();
    let from: Point<i32, Physical> =
        Point::<i32, Logical>::from((-geo.loc.x, -geo.loc.y)).to_physical_precise_round(scale);
    let size = (
        f64::from(geo.size.w) * open.scale,
        f64::from(geo.size.h) * open.scale,
    );
    let place: Point<i32, Logical> = (
        surface_loc.x + geo.loc.x + ((f64::from(geo.size.w) - size.0) / 2.0).round() as i32,
        surface_loc.y
            + geo.loc.y
            + ((f64::from(geo.size.h) - size.1) / 2.0).round() as i32
            + open.dy,
    )
        .into();
    let slot: Point<i32, Physical> = place.to_physical_precise_round(scale);
    let rounding = if is_fullscreen(window) {
        None
    } else {
        Rounding::new(
            renderer,
            Rectangle::new(place, (size.0.round() as i32, size.1.round() as i32).into()),
            (f64::from(radius) * open.scale).round() as i32,
        )
    };
    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        window.render_elements(&mut *renderer, from, scale, open.alpha);
    for surface in surfaces {
        let shrunk = RescaleRenderElement::from_element(
            surface,
            Point::<i32, Physical>::from((0, 0)),
            open.scale,
        );
        let placed = RelocateRenderElement::from_element(shrunk, slot, Relocate::Relative);
        out.push(match &rounding {
            Some(rounding) => rounding.wrap_thumb(placed),
            None => JffElement::Thumb(placed),
        });
    }
}
