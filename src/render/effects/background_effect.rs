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
use smithay::backend::renderer::element::{AsRenderElements, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
use smithay::desktop::{PopupManager, Window};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};
use smithay::wayland::compositor::{with_states, SurfaceData};

use super::blur::BlurOptions;
use super::framebuffer_effect::{FramebufferEffect, FramebufferEffectElement, WallpaperSnapshot};
use crate::config;
use crate::primitives::region::TransformedRegion;
use crate::protocol::background_effect::get_cached_blur_region;
use crate::render::elements::JffElement;

#[derive(Debug)]
pub struct RenderParams {
    /// Visible surface rectangle, in frame coordinates.
    pub geometry: Rectangle<f64, Logical>,
    /// Blurred parts of `geometry`, in the same coordinates with the region offset applied.
    pub subregion: TransformedRegion,
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
    render_for_surface_zoomed(surface, surface_loc, 1.0, fallback_geo, scale, blur, push);
}

/// [`render_for_surface`] for a surface drawn shrunk (overview thumbnail).
///
/// `zoom` scales the surface around its own origin (1.0 = real size), `surface_loc` is where
/// that origin lands in the frame. Blur area and the client region are scaled by `zoom` too,
/// so the blur covers the shrunk window.
pub fn render_for_surface_zoomed(
    surface: &WlSurface,
    surface_loc: Point<f64, Logical>,
    zoom: f64,
    fallback_geo: Option<Rectangle<i32, Logical>>,
    scale: f64,
    blur: config::Blur,
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
/// the wallpaper. `blur_whole`: also blur popups that sent no region.
pub fn popup_elements(
    renderer: &mut GlesRenderer,
    parent: &WlSurface,
    parent_loc: Point<i32, Logical>,
    parent_geo_loc: Point<i32, Logical>,
    blur: Option<config::Blur>,
    blur_whole: bool,
    out: &mut Vec<JffElement>,
) {
    let scale = Scale::from(1.0f64);
    for (popup, popup_offset) in PopupManager::popups_for_surface(parent) {
        let origin = parent_loc + parent_geo_loc + popup_offset - popup.geometry().loc;
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
            render_for_surface(
                popup.wl_surface(),
                origin.to_f64(),
                blur_whole.then_some(popup.geometry()),
                1.0,
                blur,
                &mut |element| out.push(JffElement::from(element)),
            );
        }
    }
}

/// Elements of a window: its popups, then the window itself, each followed (= drawn right
/// behind it) by its blur.
///
/// `surface_loc` is the window surface origin in the frame. `snapshot`: the window blurs a
/// wallpaper copy instead of the live frame (see `WallpaperSnapshot`).
pub fn window_elements(
    renderer: &mut GlesRenderer,
    window: &Window,
    surface_loc: Point<i32, Logical>,
    blur: Option<config::Blur>,
    snapshot: Option<&WallpaperSnapshot>,
    out: &mut Vec<JffElement>,
) {
    let scale = Scale::from(1.0f64);
    let loc: Point<i32, Physical> = surface_loc.to_physical_precise_round(scale);
    // Without blur (or for an X11 window) there's nothing to interleave.
    let (Some(blur), Some(toplevel)) = (blur, window.toplevel()) else {
        let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            window.render_elements(&mut *renderer, loc, scale, 1.0);
        out.extend(surfaces.into_iter().map(JffElement::from));
        return;
    };
    let geo = window.geometry();
    popup_elements(
        renderer,
        toplevel.wl_surface(),
        surface_loc,
        geo.loc,
        Some(blur),
        blur.all_windows,
        out,
    );
    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        render_elements_from_surface_tree(
            &mut *renderer,
            toplevel.wl_surface(),
            loc,
            scale,
            1.0,
            Kind::Unspecified,
        );
    out.extend(surfaces.into_iter().map(JffElement::from));
    render_for_surface(
        toplevel.wl_surface(),
        surface_loc.to_f64(),
        blur.all_windows.then_some(geo),
        1.0,
        blur,
        &mut |element| out.push(JffElement::from(element.with_wallpaper_source(snapshot))),
    );
}
