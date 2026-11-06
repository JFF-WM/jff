//! `ext-session-lock-v1`: how a screen locker (swaylock, hyprlock, gtklock, ...) locks the
//! session.
//!
//! Smithay does the protocol, this file says what "locked" means.
//!
//! Nothing else is shown. On every monitor jff draws the locker's lock surface (and the cursor)
//! on a black background: no window, no panel, no popup, no overview, no input-method popup. A
//! monitor without a lock surface yet is black.
//!
//! Nothing else gets input. The keyboard goes to a lock surface, the pointer can only reach one.
//! Key and mouse bindings are off, with one exception: `Ctrl+Alt+F1..F12` (virtual terminal
//! switching) still works, since it's the way out when a locker misbehaves.
//!
//! Nothing can be read from the screen. Screenshots and screen capture are refused.
//!
//! The lock holds. It ends when the locker asks to unlock. A locker that dies without asking
//! leaves the session locked, as the protocol demands (the way out is a virtual terminal). A
//! second client that asks to lock while the session is locked is told it can't.
//!
//! Confirmation. The locker is told "locked" on the event-loop iteration after the request, by
//! which time the screen is being drawn with nothing but the lock.
//!
//! Opening the overview, an area screenshot, menus, and drags are ended when the lock starts.

use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Logical, Physical, Point, Scale, SERIAL_COUNTER};
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};

use crate::feature::outputs::output_rect;
use crate::render::elements::JffElement;
use crate::state::JffState;

/// What a locked monitor is cleared to.
pub const LOCKED_BACKGROUND: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// Everything the protocol needs to keep between requests.
pub struct SessionLock {
    /// Only has to stay alive: it owns the global.
    #[allow(dead_code)]
    manager: SessionLockManagerState,
    locked: bool,
    /// The locker is waiting to be told the session is locked.
    confirmation: Option<SessionLocker>,
    /// The lock surfaces, and the monitor each one covers.
    surfaces: Vec<(LockSurface, Output)>,
}

impl SessionLock {
    /// Create the protocol global.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            manager: SessionLockManagerState::new::<JffState, _>(display, |_client| true),
            locked: false,
            confirmation: None,
            surfaces: Vec::new(),
        }
    }

    /// Whether the session is locked.
    pub fn is_locked(&self) -> bool {
        self.locked
    }
}

/// Render elements of the lock surface on `output` (nothing when it has none), in that
/// monitor's coordinates.
pub fn lock_surface_elements(
    renderer: &mut GlesRenderer,
    lock: &SessionLock,
    output: &Output,
    scale: Scale<f64>,
) -> Vec<JffElement> {
    let mut out = Vec::new();
    for (surface, on) in lock.surfaces.iter().rev() {
        if on != output || !surface.alive() {
            continue;
        }
        let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                &mut *renderer,
                surface.wl_surface(),
                Point::<i32, Physical>::from((0, 0)),
                scale,
                1.0,
                Kind::Unspecified,
            );
        out.extend(elements.into_iter().map(JffElement::from));
    }
    out
}

impl JffState {
    /// Whether the session is locked.
    pub fn session_locked(&self) -> bool {
        self.session_lock.locked
    }

    /// The lock surface that gets the keyboard: the one on the monitor with the pointer, else
    /// the first one that exists.
    pub fn lock_keyboard_surface(&self) -> Option<WlSurface> {
        let pointer = self.seat.get_pointer().map(|p| p.current_location());
        let live = || self.session_lock.surfaces.iter().filter(|(s, _)| s.alive());
        pointer
            .and_then(|pos| {
                live().find(|(_, output)| {
                    crate::primitives::layout::contains(output_rect(output), (pos.x, pos.y))
                })
            })
            .or_else(|| live().next())
            .map(|(surface, _)| surface.wl_surface().clone())
    }

    /// The lock surface under `pos` and its origin, while the session is locked: the only thing
    /// the pointer can reach then.
    pub fn lock_surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.session_lock
            .surfaces
            .iter()
            .rev()
            .filter(|(surface, _)| surface.alive())
            .find(|(_, output)| {
                crate::primitives::layout::contains(output_rect(output), (pos.x, pos.y))
            })
            .map(|(surface, output)| {
                let rect = output_rect(output);
                (
                    surface.wl_surface().clone(),
                    Point::from((rect.0 as f64, rect.1 as f64)),
                )
            })
    }

    /// Once per event-loop iteration: tells the locker it's locked, forgets dead lock surfaces.
    pub fn service_session_lock(&mut self) {
        let lock = &mut self.session_lock;
        if !lock.locked {
            return;
        }
        let before = lock.surfaces.len();
        lock.surfaces.retain(|(surface, _)| surface.alive());
        let changed = lock.surfaces.len() != before;
        if let Some(confirmation) = lock.confirmation.take() {
            confirmation.lock();
        }
        if changed {
            self.update_keyboard_focus();
        }
    }
}

impl SessionLockHandler for JffState {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock.manager
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // Already locked: dropping `confirmation` tells this client it can't lock.
        if self.session_lock.locked {
            return;
        }
        tracing::info!("session locked");
        // Let go of everything that holds input or shows windows.
        self.cancel_screenshot();
        self.abort_overview();
        self.dismiss_popups();
        if let Some(pointer) = self.seat.get_pointer() {
            if pointer.is_grabbed() {
                pointer.unset_grab(
                    self,
                    SERIAL_COUNTER.next_serial(),
                    smithay::backend::input::InputTime::now(),
                );
            }
        }
        self.session_lock.locked = true;
        self.session_lock.confirmation = Some(confirmation);
        self.update_keyboard_focus();
        self.refresh_pointer_focus();
    }

    fn unlock(&mut self) {
        if !self.session_lock.locked {
            return;
        }
        tracing::info!("session unlocked");
        self.session_lock.locked = false;
        self.session_lock.confirmation = None;
        self.session_lock.surfaces.clear();
        self.update_keyboard_focus();
        self.refresh_pointer_focus();
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        let Some(output) = Output::from_resource(&wl_output) else {
            return;
        };
        let (_, _, width, height) = output_rect(&output);
        surface.with_pending_state(|state| {
            state.size = Some((width.max(1) as u32, height.max(1) as u32).into());
        });
        surface.send_configure();
        self.session_lock.surfaces.push((surface, output));
        self.update_keyboard_focus();
        self.refresh_pointer_focus();
    }
}
