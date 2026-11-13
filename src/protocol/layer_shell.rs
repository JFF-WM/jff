//! Layer-shell (`wlr-layer-shell`): panels, bars, launchers, wallpapers, notification popups.
//!
//! Layer surfaces belong to one monitor and live in that monitor's `LayerMap` (Smithay does the
//! anchoring, margins, and exclusive-zone arithmetic). Four layers, drawn back to front:
//!
//! `Background` and `Bottom` are behind every window (wallpapers, desktop widgets). `Top` and
//! `Overlay` are in front of every window (bars, launchers, notifications, lock screens).
//!
//! What this module adds on top of the map:
//!
//! The protocol handler (map, configure, unmap surfaces), drawing (`layer_elements`) and frame
//! callbacks, pointer hit-testing (`JffState::under`) so clicks and hover reach panels, keyboard
//! focus for surfaces that ask for it (launchers: `exclusive`, clickable bars: `on_demand`),
//! exclusive zones, so a maximized window stops at a panel instead of covering it, and popups of
//! layer surfaces (menus of a bar) are kept on the monitor.

use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::utils::under_from_surface_tree;
use smithay::desktop::{
    find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output, LayerSurface,
    PopupKind, PopupManager, Window, WindowSurfaceType,
};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::{
    KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData,
    WlrLayerShellHandler, WlrLayerShellState,
};
use smithay::wayland::shell::xdg::PopupSurface;

use crate::config;
use crate::desk::Desk;
use crate::feature::outputs::output_rect;
use crate::primitives::alpha;
use crate::primitives::geom;
use crate::primitives::layout::{self, Rect};
use crate::render::effects::background_effect::{popup_elements, render_for_surface};
use crate::render::effects::gamut;
use crate::render::effects::surface_alpha::window_multiplier;
use crate::render::elements::JffElement;
use crate::state::JffState;

/// Layers drawn (and hit-tested) in front of windows, topmost first.
pub const ABOVE_WINDOWS: [Layer; 2] = [Layer::Overlay, Layer::Top];
/// What stays in front of a fullscreen window: only the overlay layer (lock screens,
/// notifications that really must show). The top layer is covered.
pub const OVERLAY_ONLY: [Layer; 1] = [Layer::Overlay];

/// Whether the topmost window on the monitor at `rect` is fullscreen *and* its committed
/// geometry is exactly the monitor. Only then does it hide everything behind it, so the render
/// path can leave those elements out and let `DrmCompositor` put the window's buffer straight on
/// the primary plane (direct scanout). A window that was asked to go fullscreen but still has
/// its old size doesn't count: the windows behind it are still visible. Neither does a window
/// its client faded (wp_alpha_modifier_v1): what is behind shows through, so the compositor has
/// to blend it.
pub fn fullscreen_covers(desk: &Desk<Window>, rect: Rect) -> bool {
    let Some(entry) = desk.top_down().find(|entry| {
        let size = entry.window.geometry().size;
        layout::intersects((entry.pos.0, entry.pos.1, size.w, size.h), rect)
    }) else {
        return false;
    };
    let size = entry.window.geometry().size;
    if (entry.pos.0, entry.pos.1, size.w, size.h) != rect {
        return false;
    }
    alpha::is_opaque(window_multiplier(&entry.window))
        && entry.window.toplevel().is_some_and(|toplevel| {
            toplevel
                .with_pending_state(|state| state.states.contains(xdg_toplevel::State::Fullscreen))
        })
}

/// The surface of the fullscreen window covering the monitor at `rect` (see `fullscreen_covers`), `None` without one and for windows that are not xdg toplevels.
pub fn fullscreen_surface(desk: &Desk<Window>, rect: Rect) -> Option<WlSurface> {
    if !fullscreen_covers(desk, rect) {
        return None;
    }
    let entry = desk.top_down().find(|entry| {
        let size = entry.window.geometry().size;
        layout::intersects((entry.pos.0, entry.pos.1, size.w, size.h), rect)
    })?;
    entry
        .window
        .toplevel()
        .map(|toplevel| toplevel.wl_surface().clone())
}

/// Whether the fullscreen window covering the monitor at `rect` (see `fullscreen_covers`) says
/// its content is a game (wp_content_type_v1). False without a covering window, and for windows
/// that never set a content type, like X11 ones.
pub fn fullscreen_says_game(desk: &Desk<Window>, rect: Rect) -> bool {
    if !fullscreen_covers(desk, rect) {
        return false;
    }
    let Some(entry) = desk.top_down().find(|entry| {
        let size = entry.window.geometry().size;
        layout::intersects((entry.pos.0, entry.pos.1, size.w, size.h), rect)
    }) else {
        return false;
    };
    entry
        .window
        .toplevel()
        .is_some_and(|toplevel| crate::protocol::content_type::says_game(toplevel.wl_surface()))
}

/// Whether the fullscreen window covering the monitor at `rect` (see `fullscreen_covers`) asked
/// to be shown with tearing (wp_tearing_control_v1). False without a covering window, and for
/// windows that never asked, like X11 ones.
pub fn fullscreen_wants_tearing(desk: &Desk<Window>, rect: Rect) -> bool {
    if !fullscreen_covers(desk, rect) {
        return false;
    }
    let Some(entry) = desk.top_down().find(|entry| {
        let size = entry.window.geometry().size;
        layout::intersects((entry.pos.0, entry.pos.1, size.w, size.h), rect)
    }) else {
        return false;
    };
    entry.window.toplevel().is_some_and(|toplevel| {
        crate::protocol::tearing_control::wants_tearing(toplevel.wl_surface())
    })
}

/// Layers to draw in front of the windows on the monitor at `rect`. With a fullscreen window on
/// top there (`fullscreen` is `JffState::fullscreen_rects`), only the overlay layer, so panels
/// don't cover the game. The overview always shows everything.
pub fn layers_above(fullscreen: &[Rect], rect: Rect, overview: bool) -> &'static [Layer] {
    if !overview && fullscreen.contains(&rect) {
        &OVERLAY_ONLY
    } else {
        &ABOVE_WINDOWS
    }
}

/// Layers drawn (and hit-tested) behind windows, topmost first.
pub const BELOW_WINDOWS: [Layer; 2] = [Layer::Bottom, Layer::Background];

/// What's under a point: a window, or a layer surface (a panel, launcher, wallpaper). Each
/// carries the surface that gets the pointer and where that surface's origin is, in the shared
/// desktop space.
pub enum Under {
    Window(Window, WlSurface, Point<f64, Logical>),
    Layer(LayerSurface, WlSurface, Point<f64, Logical>),
}

/// Whether the surface has a buffer attached, which is what "mapped" means for it.
pub(crate) fn is_mapped(surface: &WlSurface) -> bool {
    with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false)
}

/// Whether `output` has a mapped layer surface on one of `layers`.
pub fn layers_shown(output: &Output, layers: &[Layer]) -> bool {
    let map = layer_map_for_output(output);
    layers.iter().any(|layer| {
        map.layers_on(*layer)
            .any(|surface| is_mapped(surface.wl_surface()))
    })
}

/// Render elements of the layer surfaces of `output` on one of `layers`, front to back.
///
/// `blur`: `Some` draws the blurred background of surfaces that asked for one, right behind
/// them. Pass `None` for no blur (screenshots, blur off).
pub fn layer_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    layers: &[Layer],
    blur: Option<config::Blur>,
    ignore: &[String],
    popup_radius: i32,
) -> Vec<JffElement> {
    let scale = Scale::from(1.0f64);
    let map = layer_map_for_output(output);
    let mut out: Vec<JffElement> = Vec::new();
    for layer in layers {
        // Later surfaces of a layer are drawn on top of earlier ones.
        for surface in map.layers_on(*layer).rev() {
            let Some(geo) = map.layer_geometry(surface) else {
                continue;
            };
            // Layer geometry is relative to the monitor, like the frame being drawn.
            let loc: Point<i32, Physical> = geo.loc.to_physical_precise_round(scale);
            // A layer on the ignore list (`blur { ignore-layer "…" }`) gets no blur at all.
            let blur = blur.filter(|_| !ignore.iter().any(|n| n == surface.namespace()));
            // Popups in front, each with its blur right behind it.
            popup_elements(
                &mut *renderer,
                surface.wl_surface(),
                geo.loc,
                (0, 0).into(),
                blur,
                blur.is_some_and(|b| b.all_layers),
                popup_radius,
                &mut out,
            );
            let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    &mut *renderer,
                    surface.wl_surface(),
                    loc,
                    scale,
                    1.0,
                    Kind::Unspecified,
                );
            gamut::wrap_surfaces(&mut *renderer, surface.wl_surface(), elements, &mut out);
            push_layer_blur(&mut out, surface.wl_surface(), geo, *layer, blur);
        }
    }
    out
}

/// The blur behind a layer surface, pushed after (behind) its own elements.
fn push_layer_blur(
    out: &mut Vec<JffElement>,
    surface: &WlSurface,
    geo: Rectangle<i32, Logical>,
    layer: Layer,
    blur: Option<config::Blur>,
) {
    let Some(blur) = blur else {
        return;
    };
    // Without a region of its own, `all-layers` blurs panels and launchers whole. Not the
    // wallpaper or `bottom` widgets: nothing worth blurring is behind them.
    let on_blurred_layer = matches!(layer, Layer::Top | Layer::Overlay);
    let fallback = (blur.all_layers && on_blurred_layer).then(|| Rectangle::from_size(geo.size));
    render_for_surface(
        surface,
        geo.loc.to_f64(),
        fallback,
        1.0,
        blur,
        &mut |element| {
            out.push(JffElement::from(element));
        },
    );
}

/// Like `layer_elements` but only the layer surfaces themselves, without their menus.
pub fn layer_body_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    layers: &[Layer],
    blur: Option<config::Blur>,
    ignore: &[String],
) -> Vec<JffElement> {
    let scale = Scale::from(1.0f64);
    let map = layer_map_for_output(output);
    let mut out: Vec<JffElement> = Vec::new();
    for layer in layers {
        for surface in map.layers_on(*layer).rev() {
            let Some(geo) = map.layer_geometry(surface) else {
                continue;
            };
            let blur = blur.filter(|_| !ignore.iter().any(|n| n == surface.namespace()));
            let loc: Point<i32, Physical> = geo.loc.to_physical_precise_round(scale);
            let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    &mut *renderer,
                    surface.wl_surface(),
                    loc,
                    scale,
                    1.0,
                    Kind::Unspecified,
                );
            gamut::wrap_surfaces(&mut *renderer, surface.wl_surface(), elements, &mut out);
            push_layer_blur(&mut out, surface.wl_surface(), geo, *layer, blur);
        }
    }
    out
}

/// Only the menus (popovers) of the layer surfaces on `layers`, their blur shaped with
/// `popup_radius` (the config's `popup-radius`). A menu belongs in front of
/// everything, even when its panel sits behind the windows or the overview.
pub fn layer_popup_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    layers: &[Layer],
    blur: Option<config::Blur>,
    ignore: &[String],
    popup_radius: i32,
) -> Vec<JffElement> {
    let map = layer_map_for_output(output);
    let mut out: Vec<JffElement> = Vec::new();
    for layer in layers {
        for surface in map.layers_on(*layer).rev() {
            let Some(geo) = map.layer_geometry(surface) else {
                continue;
            };
            let blur = blur.filter(|_| !ignore.iter().any(|n| n == surface.namespace()));
            popup_elements(
                &mut *renderer,
                surface.wl_surface(),
                geo.loc,
                (0, 0).into(),
                blur,
                blur.is_some_and(|b| b.all_layers),
                popup_radius,
                &mut out,
            );
        }
    }
    out
}

impl WlrLayerShellHandler for JffState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        // The client may name a monitor, else the surface goes where the pointer is.
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .filter(|output| self.outputs.contains(output))
            .or_else(|| self.output_at_pointer());
        let Some(output) = output else {
            tracing::warn!("no monitor for the new layer surface {namespace:?}, closing it");
            surface.send_close();
            return;
        };
        tracing::info!(
            "new layer surface {namespace:?} on {layer:?} of {}",
            output.name()
        );

        let result =
            layer_map_for_output(&output).map_layer(&LayerSurface::new(surface.clone(), namespace));
        if let Err(err) = result {
            tracing::warn!("could not map the layer surface: {err:?}");
            surface.send_close();
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let found = self.outputs.iter().find_map(|output| {
            let map = layer_map_for_output(output);
            let layer = map
                .layers()
                .find(|layer| layer.layer_surface() == &surface)
                .cloned();
            layer.map(|layer| (output.clone(), layer))
        });
        if let Some((output, layer)) = found {
            let zone_before = layer_map_for_output(&output).non_exclusive_zone();
            let zone_after = {
                let mut map = layer_map_for_output(&output);
                map.unmap_layer(&layer);
                map.non_exclusive_zone()
            };
            // A panel that goes away gives its space back.
            if zone_before != zone_after {
                self.relayout_outputs();
            }
        }

        // A launcher that closes hands the keyboard back to the windows.
        self.update_keyboard_focus();
    }

    // The client attached a menu to one of its layer surfaces.
    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        self.unconstrain_layer_popup(&PopupKind::Xdg(popup.clone()));
        // Placed inside the monitor now, so the first configure can go out.
        if !popup.is_initial_configure_sent() {
            if let Err(err) = popup.send_configure() {
                tracing::warn!("popup of a layer surface not configured: {err:?}");
            }
        }
    }
}

impl JffState {
    /// The monitor under the pointer, or the closest one.
    pub fn output_at_pointer(&self) -> Option<Output> {
        let rect = self.rect_at_pointer();
        self.output_for_rect(rect)
            .or_else(|| self.outputs.first().cloned())
    }

    /// The monitor that covers exactly `rect`.
    fn output_for_rect(&self, rect: Rect) -> Option<Output> {
        self.outputs
            .iter()
            .find(|output| output_rect(output) == rect)
            .cloned()
    }

    /// The part of the monitor at `rect` that panels haven't reserved for themselves, shrunk by
    /// the `gaps` setting (or the border width, if that is more) on every side: what a maximized
    /// window may use. Fullscreen windows don't call this, they cover the whole monitor.
    pub fn usable_rect(&self, rect: Rect) -> Rect {
        geom::inset_rect(self.panel_free_rect(rect), self.config.maximize_inset())
    }

    /// The part of the monitor at `rect` that panels haven't reserved, before any gap.
    pub fn panel_free_rect(&self, rect: Rect) -> Rect {
        let Some(output) = self.output_for_rect(rect) else {
            return rect;
        };
        let zone = layer_map_for_output(&output).non_exclusive_zone();
        // A map nobody arranged yet has an empty zone: nothing is reserved.
        if zone.size.w <= 0 || zone.size.h <= 0 {
            return rect;
        }
        (
            rect.0 + zone.loc.x,
            rect.1 + zone.loc.y,
            zone.size.w.min(rect.2),
            zone.size.h.min(rect.3),
        )
    }

    /// Place the layer surfaces of every monitor again (after a mode or position change).
    pub fn arrange_layers(&self) {
        for output in &self.outputs {
            layer_map_for_output(output).arrange();
        }
    }

    /// A layer surface (and the part of it) under `pos`, looking only at `layers`.
    pub fn layer_under(
        &self,
        pos: Point<f64, Logical>,
        layers: &[Layer],
    ) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        for output in &self.outputs {
            let rect = output_rect(output);
            if !layout::contains(rect, (pos.x, pos.y)) {
                continue;
            }
            let origin: Point<i32, Logical> = (rect.0, rect.1).into();
            let local = pos - origin.to_f64();
            let map = layer_map_for_output(output);
            for layer in layers {
                for surface in map.layers_on(*layer).rev() {
                    let Some(geo) = map.layer_geometry(surface) else {
                        continue;
                    };
                    let inside = local - geo.loc.to_f64();
                    if let Some((wl_surface, loc)) =
                        surface.surface_under(inside, WindowSurfaceType::ALL)
                    {
                        return Some((
                            surface.clone(),
                            wl_surface,
                            (loc + geo.loc + origin).to_f64(),
                        ));
                    }
                }
            }
        }
        None
    }

    /// Like `layer_under` but only menus of layer surfaces count.
    pub fn layer_popup_under(
        &self,
        pos: Point<f64, Logical>,
        layers: &[Layer],
    ) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        for output in &self.outputs {
            let rect = output_rect(output);
            if !layout::contains(rect, (pos.x, pos.y)) {
                continue;
            }
            let origin: Point<i32, Logical> = (rect.0, rect.1).into();
            let local = pos - origin.to_f64();
            let map = layer_map_for_output(output);
            for layer in layers {
                for surface in map.layers_on(*layer).rev() {
                    let Some(geo) = map.layer_geometry(surface) else {
                        continue;
                    };
                    let inside = local - geo.loc.to_f64();
                    for (popup, popup_offset) in
                        PopupManager::popups_for_surface(surface.wl_surface())
                    {
                        let offset = popup_offset - popup.geometry().loc;
                        if let Some((wl_surface, loc)) = under_from_surface_tree(
                            popup.wl_surface(),
                            inside - offset.to_f64(),
                            (0, 0),
                            WindowSurfaceType::ALL,
                        ) {
                            return Some((
                                surface.clone(),
                                wl_surface,
                                (loc + offset + geo.loc + origin).to_f64(),
                            ));
                        }
                    }
                }
            }
        }
        None
    }

    /// The monitors whose topmost window is fullscreen.
    pub fn fullscreen_rects(&self) -> Vec<Rect> {
        self.output_rects()
            .into_iter()
            .filter(|rect| {
                self.desk
                    .top_down()
                    .find(|entry| {
                        let size = entry.window.geometry().size;
                        layout::intersects((entry.pos.0, entry.pos.1, size.w, size.h), *rect)
                    })
                    .and_then(|entry| entry.window.toplevel())
                    .is_some_and(|toplevel| {
                        toplevel.with_pending_state(|state| {
                            state.states.contains(xdg_toplevel::State::Fullscreen)
                        })
                    })
            })
            .collect()
    }

    /// What's at `pos`, front to back: overlay and top layers, windows, bottom and background
    /// layers.
    pub fn under(&self, pos: Point<f64, Logical>) -> Option<Under> {
        // Layers a fullscreen window covers can't take the pointer either.
        let fullscreen = self.fullscreen_rects();
        let above: &[Layer] = match fullscreen
            .iter()
            .find(|rect| layout::contains(**rect, (pos.x, pos.y)))
        {
            Some(rect) => layers_above(&fullscreen, *rect, false),
            None => &ABOVE_WINDOWS,
        };
        if let Some((layer, surface, loc)) = self.layer_under(pos, above) {
            return Some(Under::Layer(layer, surface, loc));
        }
        // Menus of panels behind the windows are still in front of them.
        if let Some((layer, surface, loc)) = self.layer_popup_under(pos, &BELOW_WINDOWS) {
            return Some(Under::Layer(layer, surface, loc));
        }
        if let Some((window, surface, loc)) = self.surface_under(pos) {
            return Some(Under::Window(window, surface, loc));
        }
        self.layer_under(pos, &BELOW_WINDOWS)
            .map(|(layer, surface, loc)| Under::Layer(layer, surface, loc))
    }

    /// The surface that should get the pointer at `pos`, and where its origin is.
    pub fn pointer_focus_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        // Locked session: only the screen locker can be reached.
        if self.session_locked() {
            return self.lock_surface_under(pos);
        }
        self.under(pos).map(|under| match under {
            Under::Window(_, surface, loc) | Under::Layer(_, surface, loc) => (surface, loc),
        })
    }

    /// A mapped layer surface on top of the windows that wants every key (a launcher), if any.
    pub fn exclusive_layer_surface(&self) -> Option<WlSurface> {
        for output in &self.outputs {
            let map = layer_map_for_output(output);
            for layer in ABOVE_WINDOWS {
                let found = map.layers_on(layer).rev().find(|surface| {
                    surface.cached_state().keyboard_interactivity
                        == KeyboardInteractivity::Exclusive
                        && is_mapped(surface.wl_surface())
                });
                if let Some(surface) = found {
                    return Some(surface.wl_surface().clone());
                }
            }
        }
        None
    }

    /// A click landed on a layer surface: give it the keyboard if it takes keys.
    pub fn click_layer(&mut self, layer: &LayerSurface) {
        let wants_keys = layer.cached_state().keyboard_interactivity != KeyboardInteractivity::None;
        if wants_keys {
            self.focus_layer_on_demand(layer.wl_surface());
        }
    }

    /// A commit on `root` (the root of a surface tree). Returns whether it's a layer surface.
    ///
    /// Arranges the monitor's layers (which also answers clients that changed their size or
    /// anchors), sends the initial configure, and applies the keyboard and exclusive-zone side
    /// effects.
    pub fn layer_commit(&mut self, root: &WlSurface) -> bool {
        let Some(output) = self
            .outputs
            .iter()
            .find(|output| {
                layer_map_for_output(output)
                    .layer_for_surface(root, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .cloned()
        else {
            return false;
        };

        // Arrange before the first configure, so it respects the size the client asked for.
        let (layer, zone_changed) = {
            let mut map = layer_map_for_output(&output);
            let before = map.non_exclusive_zone();
            map.arrange();
            let after = map.non_exclusive_zone();
            let layer = map
                .layer_for_surface(root, WindowSurfaceType::TOPLEVEL)
                .cloned();
            (layer, before != after)
        };
        let Some(layer) = layer else {
            return true;
        };

        let configured = with_states(root, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .map(|data| data.lock().unwrap().initial_configure_sent)
                .unwrap_or(true)
        });
        if !configured {
            layer.layer_surface().send_configure();
        }

        // A launcher that asks for every key gets it, one that hid itself (a null commit) hands
        // it back. Either way the keyboard focus is worked out again from what's mapped now.
        self.update_keyboard_focus();

        // A panel that appeared, moved, or resized changes the room maximized windows have.
        if zone_changed {
            self.relayout_outputs();
        }
        true
    }

    /// Keep a popup of a layer surface on its monitor, like windows' popups. No-op for popups
    /// that don't belong to a layer surface.
    pub fn unconstrain_layer_popup(&self, popup: &PopupKind) {
        let PopupKind::Xdg(xdg) = popup else {
            return;
        };
        let Ok(root) = find_popup_root_surface(popup) else {
            return;
        };
        for output in &self.outputs {
            let map = layer_map_for_output(output);
            let Some(layer) = map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL) else {
                continue;
            };
            let Some(geo) = map.layer_geometry(layer) else {
                return;
            };

            // The positioner works relative to its parent, so express the monitor in those
            // terms. Popups of the layers behind windows stay out of the panels' space.
            let rect = output_rect(output);
            let mut target = Rectangle::<i32, Logical>::from_size((rect.2, rect.3).into());
            if matches!(layer.layer(), Layer::Background | Layer::Bottom) {
                target = map.non_exclusive_zone();
            }
            target.loc -= geo.loc;
            target.loc -= get_popup_toplevel_coords(popup);

            xdg.with_pending_state(|state| {
                state.geometry = state.positioner.get_unconstrained_geometry(target);
            });
            return;
        }
    }
}
