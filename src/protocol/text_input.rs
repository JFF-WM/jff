//! `zwp_text_input_manager_v3` and `zwp_input_method_manager_v2`: how an on-screen keyboard or
//! input method (fcitx5, ibus, ...) types into a window, and how a window tells it where the
//! text cursor is.
//!
//! Smithay does the protocols: text input focus follows keyboard focus on its own, and the
//! input method's keyboard grab is installed on the seat's keyboard when it asks for one. This
//! file says two things. Any client may be an input method (the protocol lets one own the seat
//! at a time, a second is told it's unavailable). And an input method may show a popup
//! (`zwp_input_popup_surface_v2`) next to the text cursor, it's placed just below the cursor
//! rect the window reported, moved above when it would leave the monitor, and drawn in front of
//! windows, panels, and the overview, behind the cursor and any drag icon.

use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::{layer_map_for_output, Window, WindowSurfaceType};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};
use smithay::wayland::input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface};
use smithay::wayland::text_input::{TextInputActivation, TextInputManagerState};

use crate::desk::Desk;
use crate::feature::outputs::output_rect;
use crate::primitives::layout::Rect;
use crate::render::elements::JffElement;
use crate::state::JffState;

/// Everything the two protocols need to keep between requests.
pub struct TextInput {
    /// Only has to stay alive: they own the globals.
    #[allow(dead_code)]
    text_input_manager: TextInputManagerState,
    #[allow(dead_code)]
    input_method_manager: InputMethodManagerState,
    /// Candidate windows input methods have open, oldest first.
    popups: Vec<PopupSurface>,
}

impl TextInput {
    /// Create the protocol globals.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            text_input_manager: TextInputManagerState::new::<JffState>(display),
            input_method_manager: InputMethodManagerState::new::<JffState, _>(display, |_client| {
                true
            }),
            popups: Vec::new(),
        }
    }

    /// The candidate windows that are still there.
    pub fn popups(&self) -> &[PopupSurface] {
        &self.popups
    }
}

fn is_toplevel_of(window: &Window, surface: &WlSurface) -> bool {
    window
        .toplevel()
        .map(|t| t.wl_surface() == surface)
        .unwrap_or(false)
}

/// Where `surface`'s origin is (a window, or a layer surface like a launcher), in desktop
/// coordinates, plus its size.
fn surface_rect(
    desk: &Desk<Window>,
    outputs: &[Output],
    surface: &WlSurface,
) -> Option<Rectangle<i32, Logical>> {
    if let Some(entry) = desk.find_by(|w| is_toplevel_of(w, surface)) {
        // `pos` is where the window's geometry starts, the surface starts `geometry.loc` before.
        let geo = entry.window.geometry();
        let origin: Point<i32, Logical> = Point::from(entry.pos) - geo.loc;
        return Some(Rectangle::new(origin, geo.size));
    }
    for output in outputs {
        let map = layer_map_for_output(output);
        let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) else {
            continue;
        };
        let geo = map.layer_geometry(layer)?;
        let rect = output_rect(output);
        let origin: Point<i32, Logical> = Point::from((rect.0, rect.1)) + geo.loc;
        return Some(Rectangle::new(origin, geo.size));
    }
    None
}

// jff has nothing to do when a text field gains or loses its input method.
impl TextInputActivation for JffState {}

impl InputMethodHandler for JffState {
    fn new_popup(&mut self, surface: PopupSurface) {
        self.text_input.popups.retain(|p| p.alive());
        self.text_input.popups.push(surface);
    }

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        self.text_input.popups.retain(|p| *p != surface);
    }

    // The next frame places it again from the new cursor rect, nothing to keep.
    fn popup_repositioned(&mut self, _surface: PopupSurface) {}

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        surface_rect(&self.desk, &self.outputs, parent).unwrap_or_default()
    }
}

/// A candidate window and where its top-left goes, in desktop coordinates.
pub struct PlacedPopup {
    surface: WlSurface,
    pos: Point<i32, Logical>,
}

/// Work out where candidate windows go. Free function over the fields it needs, so it can run
/// while the renderer borrows another part of the state.
pub fn place_input_popups(
    text_input: &TextInput,
    desk: &Desk<Window>,
    outputs: &[Output],
) -> Vec<PlacedPopup> {
    let mut placed = Vec::new();
    for popup in text_input.popups() {
        if !popup.alive() {
            continue;
        }
        let Some(parent) = popup.get_parent() else {
            continue;
        };
        let Some(parent_rect) = surface_rect(desk, outputs, &parent.surface) else {
            continue;
        };
        // The cursor rect is in the parent surface's coordinates.
        let cursor = popup.text_input_rectangle();
        let cursor_loc = parent_rect.loc + cursor.loc;
        let size = smithay::desktop::utils::bbox_from_surface_tree(popup.wl_surface(), (0, 0)).size;
        // Below the text cursor, above it when it wouldn't fit on the monitor.
        let mut pos = cursor_loc + Point::from((0, cursor.size.h));
        if let Some(output) = outputs.iter().find(|o| {
            let r = output_rect(o);
            crate::primitives::layout::contains(r, (cursor_loc.x as f64, cursor_loc.y as f64))
        }) {
            let (x, y, w, h) = output_rect(output);
            if pos.y + size.h > y + h {
                pos.y = cursor_loc.y - size.h;
            }
            pos.x = pos.x.min(x + w - size.w).max(x);
            pos.y = pos.y.max(y);
        }
        placed.push(PlacedPopup {
            surface: popup.wl_surface().clone(),
            pos,
        });
    }
    placed
}

/// Render elements of the candidate windows on the monitor at `rect`, front to back.
pub fn input_popup_elements(
    renderer: &mut GlesRenderer,
    placed: &[PlacedPopup],
    rect: Rect,
    scale: Scale<f64>,
) -> Vec<JffElement<GlesRenderer>> {
    let origin: Point<i32, Logical> = (rect.0, rect.1).into();
    let mut out = Vec::new();
    for popup in placed.iter().rev() {
        let size = smithay::desktop::utils::bbox_from_surface_tree(&popup.surface, (0, 0)).size;
        let area = (popup.pos.x, popup.pos.y, size.w, size.h);
        if !crate::primitives::layout::intersects(area, rect) {
            continue;
        }
        let loc: Point<i32, Physical> = (popup.pos - origin).to_physical_precise_round(scale);
        let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                &mut *renderer,
                &popup.surface,
                loc,
                scale,
                1.0,
                Kind::Unspecified,
            );
        out.extend(elements.into_iter().map(JffElement::from));
    }
    out
}
