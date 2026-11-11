//! Input: forwards winit keyboard/pointer events to the seat, and does click-to-focus.

pub mod grabs;
mod tablet;

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent,
    GestureEndEvent, GesturePinchUpdateEvent, GestureSwipeUpdateEvent, InputBackend, InputEvent,
    InputTime, KeyState, KeyboardKeyEvent, Keycode, PointerAxisEvent, PointerButtonEvent,
    PointerMotionEvent,
};
use smithay::desktop::{Window, WindowSurfaceType};
use smithay::input::keyboard::KeyboardSource;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, Focus, GestureHoldBeginEvent, GestureHoldEndEvent,
    GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent as PinchUpdate,
    GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent as SwipeUpdate,
    GrabStartData, MotionEvent, RelativeMotionEvent,
};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Point, Serial, Size, SERIAL_COUNTER};
use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraint};

use crate::config::{Action, Config, HotCorner};
use crate::desk::WindowMode;
use crate::feature::outputs::output_rect;
use crate::feature::overview_mode::{overview_key, typed_char, OverviewKey};
use crate::feature::screenshot::{report_failure, screenshot_key, ScreenshotKey};
use crate::input::grabs::r#move::{shrunk_geometry, MoveGrab};
use crate::input::grabs::resize::ResizeGrab;
use crate::ipc::ViewId;
use crate::primitives::geom;
use crate::primitives::layout;
use crate::protocol::layer_shell::Under;
use crate::protocol::xx_hotkey::mods_of;
use crate::protocol::xx_zones::ZoneDrag;
use crate::state::{configure_mode, JffState};

/// What a compositor key combination asks for.
enum Bind {
    None,
    Action(Action),
    Vt(i32),
    /// Overview is open, and acts on this key.
    Overview(OverviewKey),
    /// Area screenshot selection is open, and acts on this key.
    Screenshot(ScreenshotKey),
    /// Any other key while overview or screenshot is open: nobody gets it.
    Swallow,
    /// The release of a key a `forward-key` binding handed to this window.
    ForwardRelease(WlSurface),
}

/// Run `command` with `args` as a detached child. Also used by the IPC (`spawn` / `spawn-sh`).
pub(crate) fn spawn_command(command: &str, args: &[String]) {
    if let Err(err) = std::process::Command::new(command).args(args).spawn() {
        tracing::warn!("could not start {command}: {err}");
    }
}

fn spawn_terminal() {
    let term = std::env::var("JFF_TERMINAL")
        .or_else(|_| std::env::var("TERMINAL"))
        .unwrap_or_else(|_| "alacritty".to_string());
    if let Err(err) = std::process::Command::new(&term).spawn() {
        tracing::warn!("could not start {term}: {err}");
    }
}

pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

impl JffState {
    pub fn process_input_event<B: InputBackend>(&mut self, event: InputEvent<B>) {
        // Idle tools (ext-idle-notify) measure time since the last input.
        self.idle_notifier_state.notify_activity(&self.seat);
        // zwp_input_timestamps_v1: a client that asked gets the event's time just before the
        // event itself. Sending it for an event a compositor binding then swallows is harmless:
        // the next event of that device is stamped again, and the client keeps the latest.
        match &event {
            InputEvent::Keyboard { event } => self.stamp_keyboard(Event::time(event)),
            InputEvent::PointerMotion { event } => self.stamp_pointer(Event::time(event)),
            InputEvent::PointerMotionAbsolute { event } => self.stamp_pointer(Event::time(event)),
            InputEvent::PointerButton { event } => self.stamp_pointer(Event::time(event)),
            InputEvent::PointerAxis { event } => self.stamp_pointer(Event::time(event)),
            _ => {}
        }
        match event {
            InputEvent::Keyboard { event } => {
                // A real key while a virtual keyboard's keymap is on the seat: back to the
                // configured one first.
                self.restore_seat_keymap();
                self.keyboard_key(
                    KeyboardSource::MAIN,
                    event.key_code(),
                    event.state(),
                    Event::time(&event),
                );
            }

            InputEvent::PointerMotionAbsolute { event } => {
                // Winit reports a position relative to the host window, no flip needed.
                let rects = self.output_rects();
                let (x, y, w, h) = layout::bounds(&rects);
                let size: Size<i32, Logical> = (w, h).into();
                let pos = event.position_transformed(size) + Point::from((x as f64, y as f64));
                self.pointer_motion_to(pos, event.time());
            }

            // Mice and touchpads (libinput) report how far the pointer moved, not where it is.
            InputEvent::PointerMotion { event } => {
                self.pointer_motion_relative(event.delta(), event.delta_unaccel(), event.time());
            }

            // Pens and tablets (zwp_tablet_manager_v2), see `tablet.rs`.
            InputEvent::DeviceAdded { device } => self.tablet_device_added(&device),
            InputEvent::DeviceRemoved { device } => self.tablet_device_removed(&device),
            InputEvent::TabletToolAxis { event } => self.tablet_tool_axis::<B>(&event),
            InputEvent::TabletToolProximity { event } => self.tablet_tool_proximity::<B>(&event),
            InputEvent::TabletToolTip { event } => self.tablet_tool_tip::<B>(&event),
            InputEvent::TabletToolButton { event } => self.tablet_tool_button::<B>(&event),

            InputEvent::PointerButton { event } => {
                self.pointer_button(event.button_code(), event.state(), event.time());
            }

            InputEvent::PointerAxis { event } => {
                self.hotkey_input_seen(None);
                let pointer = self.seat.get_pointer().unwrap();
                let source = event.source();
                // Over the overview itself scroll switches workspace, over a panel it goes on to the panel.
                let amount_y = event.amount(Axis::Vertical).unwrap_or_else(|| {
                    event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0
                });
                let stop_y = amount_y == 0.0
                    && (source == AxisSource::Finger || source == AxisSource::Continuous)
                    && event.amount(Axis::Vertical) == Some(0.0);
                if self.overview_scroll(source, amount_y, event.amount_v120(Axis::Vertical), stop_y)
                {
                    return;
                }
                let mut frame = AxisFrame::new(event.time()).source(source);

                for axis in [Axis::Horizontal, Axis::Vertical] {
                    let amount = event
                        .amount(axis)
                        .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0);
                    if amount != 0.0 {
                        frame = frame
                            .relative_direction(axis, event.relative_direction(axis))
                            .value(axis, amount);
                        if let Some(v120) = event.amount_v120(axis) {
                            frame = frame.v120(axis, v120 as i32);
                        }
                    } else if (source == AxisSource::Finger || source == AxisSource::Continuous)
                        && event.amount(axis) == Some(0.0)
                    {
                        frame = frame.stop(axis);
                    }
                }

                pointer.axis(self, frame);
                pointer.frame(self);
            }

            // Touchpad gestures (zwp_pointer_gestures_v1), for the window under the pointer.
            InputEvent::GestureSwipeBegin { event } => {
                let pointer = self.seat.get_pointer().unwrap();
                let (serial, time, fingers) =
                    (SERIAL_COUNTER.next_serial(), event.time(), event.fingers());
                pointer.gesture_swipe_begin(
                    self,
                    &GestureSwipeBeginEvent {
                        serial,
                        time,
                        fingers,
                    },
                );
                pointer.frame(self);
            }
            InputEvent::GestureSwipeUpdate { event } => {
                let pointer = self.seat.get_pointer().unwrap();
                let (time, delta) = (event.time(), event.delta());
                pointer.gesture_swipe_update(self, &SwipeUpdate { time, delta });
                pointer.frame(self);
            }
            InputEvent::GestureSwipeEnd { event } => {
                let pointer = self.seat.get_pointer().unwrap();
                let (serial, time, cancelled) = (
                    SERIAL_COUNTER.next_serial(),
                    event.time(),
                    event.cancelled(),
                );
                pointer.gesture_swipe_end(
                    self,
                    &GestureSwipeEndEvent {
                        serial,
                        time,
                        cancelled,
                    },
                );
                pointer.frame(self);
            }
            InputEvent::GesturePinchBegin { event } => {
                let pointer = self.seat.get_pointer().unwrap();
                let (serial, time, fingers) =
                    (SERIAL_COUNTER.next_serial(), event.time(), event.fingers());
                pointer.gesture_pinch_begin(
                    self,
                    &GesturePinchBeginEvent {
                        serial,
                        time,
                        fingers,
                    },
                );
                pointer.frame(self);
            }
            InputEvent::GesturePinchUpdate { event } => {
                let pointer = self.seat.get_pointer().unwrap();
                let update = PinchUpdate {
                    time: event.time(),
                    delta: event.delta(),
                    scale: event.scale(),
                    rotation: event.rotation(),
                };
                pointer.gesture_pinch_update(self, &update);
                pointer.frame(self);
            }
            InputEvent::GesturePinchEnd { event } => {
                let pointer = self.seat.get_pointer().unwrap();
                let (serial, time, cancelled) = (
                    SERIAL_COUNTER.next_serial(),
                    event.time(),
                    event.cancelled(),
                );
                pointer.gesture_pinch_end(
                    self,
                    &GesturePinchEndEvent {
                        serial,
                        time,
                        cancelled,
                    },
                );
                pointer.frame(self);
            }
            InputEvent::GestureHoldBegin { event } => {
                let pointer = self.seat.get_pointer().unwrap();
                let (serial, time, fingers) =
                    (SERIAL_COUNTER.next_serial(), event.time(), event.fingers());
                pointer.gesture_hold_begin(
                    self,
                    &GestureHoldBeginEvent {
                        serial,
                        time,
                        fingers,
                    },
                );
                pointer.frame(self);
            }
            InputEvent::GestureHoldEnd { event } => {
                let pointer = self.seat.get_pointer().unwrap();
                let (serial, time, cancelled) = (
                    SERIAL_COUNTER.next_serial(),
                    event.time(),
                    event.cancelled(),
                );
                pointer.gesture_hold_end(
                    self,
                    &GestureHoldEndEvent {
                        serial,
                        time,
                        cancelled,
                    },
                );
                pointer.frame(self);
            }

            _ => {}
        }
    }

    /// A key went down or up on `source`: compositor bindings, the overview and screenshot
    /// keys, then the focused client. Real keyboards use [`KeyboardSource::MAIN`], each
    /// virtual keyboard its own source (see `protocol/virtual_keyboard.rs`).
    pub(crate) fn keyboard_key(
        &mut self,
        source: KeyboardSource,
        key_code: Keycode,
        key_state: KeyState,
        time: InputTime,
    ) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();

        // Step 1: update xkb state and catch compositor keybinds. Bindings come from
        // config.kdl (see config.rs). Ctrl+Alt+F1..F12 switch VT (TTY backend only).
        let pressed = key_state == KeyState::Pressed;
        // A client that asked for the shortcuts (a game) has the keyboard: only the way
        // out of the bindings is left. Queried here, not in the closure, because the
        // keyboard is in use there.
        let inhibited = self.shortcuts_inhibited();
        // Client hotkeys: the modifiers held before this key, and what it types in every layout
        // group of the configured keymap.
        let hotkey_mods = mods_of(&keyboard.modifier_state());
        let hotkey_syms = if pressed {
            self.hotkey_key_syms(key_code)
        } else {
            Vec::new()
        };
        let Some((bind, mods_changed)) =
            keyboard.input_intercept(source, self, key_code, key_state, |state, mods, handle| {
                // While the overview is open it gets first pick, and keeps releases
                // (and every key it doesn't use) from reaching anyone.
                let overview = state.overview_active();
                let screenshot = state.screenshot.is_some();
                if !pressed {
                    state.client_hotkey_key_released(key_code);
                    // A key whose press no client saw keeps its release hidden too, or
                    // they'd get a release with no press.
                    let suppressed = state.suppressed_keys.remove(&key_code);
                    if let Some(surface) = state.forwarded_keys.remove(&key_code) {
                        return Bind::ForwardRelease(surface);
                    }
                    // Letting go of Alt picks the window the switcher is on.
                    if state.switcher_released((mods.alt, mods.ctrl, mods.logo)) {
                        return Bind::Overview(OverviewKey::Select);
                    }
                    return if overview || screenshot || suppressed {
                        Bind::Swallow
                    } else {
                        Bind::None
                    };
                }
                state.hotkey_input_seen(Some(key_code));
                let bind = (|| {
                    let raw = handle.modified_sym().raw();
                    // XF86Switch_VT_1 ..= XF86Switch_VT_12
                    if (0x1008FE01..=0x1008FE0C).contains(&raw) {
                        return Bind::Vt((raw - 0x1008FE01 + 1) as i32);
                    }
                    // Locked session: no bindings, only the VT keys above.
                    if state.session_locked() {
                        return Bind::None;
                    }
                    let sym = handle.raw_latin_sym_or_raw_current_sym();
                    if overview {
                        // The overview key again closes it. Quitting still works.
                        let configured = sym.and_then(|sym| state.config.action_for(mods, sym));
                        return match configured {
                            // Closes it, also with something typed (Escape only clears
                            // that first).
                            Some(Action::Overview) => Bind::Action(Action::Overview),
                            Some(Action::Quit) => Bind::Action(Action::Quit),
                            // Anything else that types a character goes to the search.
                            _ => {
                                let sym = handle.modified_sym();
                                overview_key(sym)
                                    .or_else(|| {
                                        typed_char(sym, mods.ctrl, mods.alt, mods.logo)
                                            .map(OverviewKey::Type)
                                    })
                                    .map_or(Bind::Swallow, Bind::Overview)
                            }
                        };
                    }
                    if screenshot {
                        // Quitting and the overview still work (opening the overview
                        // cancels the selection). The screenshot key cancels.
                        let configured = sym.and_then(|sym| state.config.action_for(mods, sym));
                        return match configured {
                            Some(Action::Quit) => Bind::Action(Action::Quit),
                            Some(Action::Overview) => Bind::Action(Action::Overview),
                            Some(Action::ScreenshotArea) => Bind::Screenshot(ScreenshotKey::Cancel),
                            _ => screenshot_key(handle.modified_sym())
                                .map_or(Bind::Swallow, Bind::Screenshot),
                        };
                    }
                    let action = sym
                        .and_then(|sym| state.config.action_for(mods, sym))
                        .filter(|action| {
                            !inhibited || matches!(action, Action::ToggleShortcutsInhibit)
                        })
                        .cloned();
                    match action {
                        Some(action) => Bind::Action(action),
                        // jff's own bindings come first, a client hotkey gets what is left.
                        None if !inhibited
                            && state.client_hotkey_pressed(key_code, hotkey_mods, &hotkey_syms) =>
                        {
                            Bind::Swallow
                        }
                        None => Bind::None,
                    }
                })();
                // Every press kept from the client is remembered, so its release is
                // kept too. A press that goes to the client clears the mark instead: an
                // old one would swallow the release of a key the client sees held down,
                // and a client repeats a held key until it is released.
                if matches!(bind, Bind::None) {
                    if state.suppressed_keys.remove(&key_code) {
                        tracing::warn!("key {key_code:?} was still marked as kept from clients");
                    }
                } else {
                    state.suppressed_keys.insert(key_code);
                }
                bind
            })
        else {
            return;
        };

        // A key kept from the client may still have changed a modifier (a binding on
        // Super itself, say). Clients need to know, or their view of the modifiers
        // drifts from ours.
        if mods_changed && !matches!(bind, Bind::None) {
            keyboard.advertise_modifier_state(self);
        }

        // A compositor shortcut: the focused client never sees it.
        match bind {
            Bind::Action(Action::ForwardKey(app_id)) => {
                if let Some(surface) = self.forward_target(&app_id) {
                    self.forwarded_keys.insert(key_code, surface.clone());
                    self.forward_key_to(&surface, key_code, key_state, serial, time);
                }
                return;
            }
            Bind::ForwardRelease(surface) => {
                self.forward_key_to(&surface, key_code, key_state, serial, time);
                return;
            }
            Bind::Action(action) => {
                self.run_action(action);
                return;
            }
            Bind::Vt(vt) => {
                self.change_vt(vt);
                return;
            }
            Bind::Overview(key) => {
                self.handle_overview_key(key);
                return;
            }
            Bind::Screenshot(key) => {
                self.handle_screenshot_key(key);
                return;
            }
            Bind::Swallow => return,
            Bind::None => {}
        }

        // Step 2: forward the key to the focused client.
        keyboard.input_forward(self, key_code, key_state, serial, time, mods_changed);
    }

    /// The pointer moved by `delta`, as a mouse or touchpad reports it (and as a
    /// `zwlr_virtual_pointer_v1` does): pointer constraints, the hot-corner barrier and the
    /// clients under the pointer are handled here.
    #[allow(clippy::clone_on_copy)]
    pub(crate) fn pointer_motion_relative(
        &mut self,
        delta: Point<f64, Logical>,
        delta_unaccel: Point<f64, Logical>,
        time: InputTime,
    ) {
        let pointer = self.seat.get_pointer().unwrap();

        // Locked to the surface that already has the pointer: the cursor can't move, so skip
        // the hit-test. Anything that changes who is under the pointer ends the shortcut.
        if let Some((surface, origin)) = self.locked_pointer_under.clone() {
            let still_locked = surface.is_alive()
                && pointer.current_focus().as_ref() == Some(&surface)
                && !pointer.is_grabbed()
                && !self.session_locked()
                && !self.overview_active()
                && self.screenshot.is_none()
                && with_pointer_constraint(&surface, &pointer, |constraint| {
                    constraint.is_some_and(|c| {
                        c.is_active() && matches!(&*c, PointerConstraint::Locked(_))
                    })
                });
            if still_locked {
                pointer.relative_motion(
                    self,
                    Some((surface, origin)),
                    &RelativeMotionEvent {
                        delta,
                        delta_unaccel,
                        time,
                    },
                );
                pointer.frame(self);
                return;
            }
            self.locked_pointer_under = None;
        }

        let cur = pointer.current_location();
        let under = self.pointer_focus_under(cur);

        // Is the surface under the pointer holding an active lock or confine?
        let mut locked = false;
        let mut confined = false;
        let mut confine_region = None;
        if let Some((surface, origin)) = under.as_ref() {
            with_pointer_constraint(surface, &pointer, |constraint| {
                if let Some(constraint) = constraint {
                    if !constraint.is_active() {
                        return;
                    }
                    let local = (cur - *origin).to_i32_round();
                    if !constraint.region().map_or(true, |r| r.contains(local)) {
                        return;
                    }
                    match &*constraint {
                        PointerConstraint::Locked(_) => locked = true,
                        PointerConstraint::Confined(c) => {
                            confined = true;
                            confine_region = c.region().cloned();
                        }
                    }
                }
            });
        }

        // Raw motion goes to zwp_relative_pointer, whatever the cursor does.
        pointer.relative_motion(
            self,
            under.clone(),
            &RelativeMotionEvent {
                delta,
                delta_unaccel,
                time: time.clone(),
            },
        );

        // Locked: the cursor stays put, the client only sees relative motion.
        if locked {
            self.locked_pointer_under = under;
            pointer.frame(self);
            return;
        }

        let rects = self.pointer_bounds();
        let pos = cur + delta;
        let pos: Point<f64, Logical> = layout::clamp_to_rects((pos.x, pos.y), &rects).into();
        let pos = self.hot_corner_barrier(cur, pos);
        let new_under = self.pointer_focus_under(pos);

        // Confined: never leave the surface (or the confine region).
        if confined {
            if let Some((surface, origin)) = under.as_ref() {
                let same = new_under.as_ref().map(|(s, _)| s) == Some(surface);
                let inside = confine_region
                    .as_ref()
                    .map_or(true, |r| r.contains((pos - *origin).to_i32_round()));
                if !same || !inside {
                    pointer.frame(self);
                    return;
                }
            }
        }

        self.check_hot_corner(pos);
        pointer.motion(
            self,
            new_under.clone(),
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
        self.update_toplevel_drag();

        // The pointer reached a surface that already asked for a lock: engage it.
        if let Some((surface, origin)) = new_under {
            with_pointer_constraint(&surface, &pointer, |constraint| {
                if let Some(constraint) = constraint {
                    if !constraint.is_active() {
                        let local = (pos - origin).to_i32_round();
                        if constraint.region().map_or(true, |r| r.contains(local)) {
                            constraint.activate();
                        }
                    }
                }
            });
        }
    }

    /// The pointer is at `pos` in the shared desktop space (winit, `zwlr_virtual_pointer_v1`
    /// absolute motion). Kept on a monitor.
    pub(crate) fn pointer_motion_to(&mut self, pos: Point<f64, Logical>, time: InputTime) {
        let pos: Point<f64, Logical> =
            layout::clamp_to_rects((pos.x, pos.y), &self.pointer_bounds()).into();
        self.check_hot_corner(pos);

        let under = self.pointer_focus_under(pos);
        let pointer = self.seat.get_pointer().unwrap();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
        self.update_toplevel_drag();
    }

    /// A pointer button went down or up: mouse bindings, click-to-focus, window drags, the
    /// overview and screenshot selection, then the client under the pointer. Real mice and
    /// `jff msg click` both come through here.
    pub(crate) fn pointer_button(&mut self, button: u32, state: ButtonState, time: InputTime) {
        self.hotkey_input_seen(None);
        // Mouse bindings (MouseBack, MouseForward): the press runs the action, and both
        // press and release are kept from clients so none sees half a click.
        match state {
            ButtonState::Pressed => {
                let mods = self
                    .seat
                    .get_keyboard()
                    .map(|k| k.modifier_state())
                    .unwrap_or_default();
                // Not while a window that asked for the shortcuts has the keyboard.
                let inhibited = self.shortcuts_inhibited() || self.session_locked();
                if let Some(action) = self
                    .config
                    .button_action_for(&mods, button)
                    .filter(|_| !inhibited)
                    .cloned()
                {
                    self.swallowed_buttons.insert(button);
                    // Like keys: with overview or screenshot open, only `overview`
                    // (which closes it) and `quit` act, anything else is ignored.
                    let modal = self.overview_active() || self.screenshot.is_some();
                    if !modal || matches!(action, Action::Overview | Action::Quit) {
                        self.run_action(action);
                    }
                    return;
                }
                // Then a client hotkey, which never acts in the overview or a selection.
                let modal = self.overview_active() || self.screenshot.is_some();
                if !inhibited && !modal && self.client_hotkey_button(button, mods_of(&mods)) {
                    self.swallowed_buttons.insert(button);
                    return;
                }
            }
            ButtonState::Released => {
                self.client_hotkey_button_released(button);
                if self.swallowed_buttons.remove(&button) {
                    return;
                }
            }
        }

        // The overview acts on clicks itself, they never reach a client.
        if self.overview_active() {
            self.overview_button(button, state, time);
            return;
        }
        // Same for the screenshot selection: no client sees the clicks that draw it.
        if self.screenshot.is_some() {
            self.screenshot_button(button, state);
            return;
        }
        let pointer = self.seat.get_pointer().unwrap();
        let serial = SERIAL_COUNTER.next_serial();

        // Click-to-focus and click-to-raise, before the click reaches the client.
        if state == ButtonState::Pressed && !self.session_locked() {
            let under = self.under(pointer.current_location());
            let at = pointer.current_location();
            self.record_click(button, (at.x, at.y), under.as_ref());
            if let Some(Under::Layer(layer, _, _)) = &under {
                // A panel or launcher was clicked: it may take the keyboard, and the
                // windows stay as they are.
                self.click_layer(layer);
            }
            if let Some(Under::Window(window, surface, _)) = under {
                tracing::debug!("button {} pressed on surface {:?}", button, surface.id());
                self.focus_window(&window);

                // Modifier+drag (Alt, see `drag-modifier`) works on any window, even
                // ones without their own titlebar: left button moves, right button
                // resizes from the nearest corner.
                let alt = self
                    .seat
                    .get_keyboard()
                    .map(|k| self.config.drag_modifier_held(&k.modifier_state()))
                    .unwrap_or(false);
                if let Some(k) = self.seat.get_keyboard() {
                    let m = k.modifier_state();
                    tracing::debug!(
                        "click on window: ctrl={} alt={} shift={} logo={} drag_modifier_held={alt}",
                        m.ctrl,
                        m.alt,
                        m.shift,
                        m.logo
                    );
                }
                // A window that asked for the shortcuts gets Alt and the clicks itself.
                if alt
                    && !self.window_inhibits_shortcuts(&window)
                    && self.start_modifier_grab(&window, button, serial)
                {
                    // The grab owns the pointer now, so the client never sees this press.
                    pointer.button(
                        self,
                        &ButtonEvent {
                            button: button,
                            state,
                            serial,
                            time: time,
                        },
                    );
                    pointer.frame(self);
                    return;
                }
            }
        }

        pointer.button(
            self,
            &ButtonEvent {
                button: button,
                state,
                serial,
                time: time,
            },
        );
        pointer.frame(self);
    }

    /// The surface of the topmost window whose app id is `app_id`, on any workspace.
    fn forward_target(&self, app_id: &str) -> Option<WlSurface> {
        self.desk.all_top_down().find_map(|entry| {
            let surface = entry.window.toplevel()?.wl_surface();
            (crate::feature::osd::toplevel_names(surface).0 == app_id).then(|| surface.clone())
        })
    }

    /// Give `surface` the keyboard for one key event, then hand it back: the app sees the key
    /// (with the modifiers held now) while the window the user works in keeps its focus.
    fn forward_key_to(
        &mut self,
        surface: &WlSurface,
        key_code: Keycode,
        key_state: KeyState,
        serial: Serial,
        time: InputTime,
    ) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let previous = keyboard.current_focus();
        if previous.as_ref() == Some(surface) {
            keyboard.input_forward(self, key_code, key_state, serial, time, false);
            return;
        }
        keyboard.set_focus(self, Some(surface.clone()), SERIAL_COUNTER.next_serial());
        keyboard.advertise_modifier_state(self);
        keyboard.input_forward(self, key_code, key_state, serial, time, false);
        keyboard.set_focus(self, previous, SERIAL_COUNTER.next_serial());
        keyboard.advertise_modifier_state(self);
    }

    /// Carry out the action of a triggered key binding.
    fn run_action(&mut self, action: Action) {
        match action {
            Action::Spawn(argv) => {
                if let Some((command, args)) = argv.split_first() {
                    spawn_command(command, args);
                }
            }
            Action::SpawnShell(line) => spawn_command("sh", &["-c".to_string(), line]),
            Action::Terminal => spawn_terminal(),
            Action::Maximize => self.toggle_maximize_focused(),
            Action::Fullscreen => self.toggle_fullscreen_focused(),
            Action::Snap(side) => self.snap_focused(side),
            Action::Close => self.close_focused(),
            Action::Overview => self.toggle_overview(),
            Action::SwitchWindow { backwards } => self.open_switcher(backwards),
            Action::Workspace(goto) => self.goto_workspace(goto),
            Action::MoveToWorkspace(workspace) => self.move_focused_to_workspace(workspace),
            Action::ReloadConfig => self.reload_config(),
            Action::ScreenshotArea => {
                if let Err(why) = self.start_screenshot_area() {
                    report_failure(&why);
                }
            }
            Action::ScreenshotFullscreen => {
                if let Err(why) = self.screenshot_fullscreen() {
                    report_failure(&why);
                }
            }
            Action::ScreenshotWindow => {
                if let Err(why) = self.screenshot_window() {
                    report_failure(&why);
                }
            }
            Action::ToggleShortcutsInhibit => self.toggle_shortcuts_inhibit(),
            // Handled where the key is, it needs the key and its state.
            Action::ForwardKey(_) => {}
            Action::Quit => self.loop_signal.stop(),
        }
    }

    /// Start a compositor-driven move (left button) or resize (right button) of `window`. Returns
    /// false when `button` isn't one of those, or the window can't be found.
    fn start_modifier_grab(
        &mut self,
        window: &Window,
        button: u32,
        serial: smithay::utils::Serial,
    ) -> bool {
        if button != BTN_LEFT && button != BTN_RIGHT {
            return false;
        }
        let pointer = self.seat.get_pointer().unwrap();
        let location = pointer.current_location();
        let Some((initial_pos, size)) = self
            .desk
            .find_by(|w| w == window)
            .map(|e| (e.pos, e.window.geometry().size))
        else {
            return false;
        };
        let start_data = GrabStartData {
            focus: None,
            button,
            location,
        };

        if button == BTN_LEFT {
            // Alt+drag on a window in a zone moves the whole zone.
            let zone = self.zone_drag_for(window);
            return self.start_move_with(window, start_data, serial, zone);
        }

        let Some(toplevel) = window.toplevel().cloned() else {
            return false;
        };
        if !self.desk.is_floating(|w| w == window) {
            self.desk.float_in_place_where(|w| w == window);
            configure_mode(
                &toplevel,
                WindowMode::Floating,
                false,
                Some((size.w, size.h)),
            );
        }
        let local = (
            location.x - initial_pos.0 as f64,
            location.y - initial_pos.1 as f64,
        );
        let edges = geom::nearest_corner(local, (size.w, size.h));
        let grab = ResizeGrab::new(start_data, toplevel, edges, initial_pos, (size.w, size.h));
        pointer.set_grab(self, grab, serial, Focus::Clear);
        true
    }

    pub fn start_move(
        &mut self,
        window: &Window,
        start_data: GrabStartData<JffState>,
        serial: smithay::utils::Serial,
    ) -> bool {
        self.start_move_with(window, start_data, serial, None)
    }

    /// [`JffState::start_move`], taking the zone of the window along when `zone` is given.
    fn start_move_with(
        &mut self,
        window: &Window,
        start_data: GrabStartData<JffState>,
        serial: smithay::utils::Serial,
        zone: Option<ZoneDrag>,
    ) -> bool {
        let Some((pos, size)) = self
            .desk
            .find_by(|w| w == window)
            .map(|e| (e.pos, e.window.geometry().size))
        else {
            return false;
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        let at = (start_data.location.x, start_data.location.y);
        let mode = self.desk.mode_of(|w| w == window);
        let mut initial_pos = pos;
        let mut back = None;

        if let Some(toplevel) = window.toplevel().cloned() {
            match mode {
                WindowMode::Maximized => {
                    let monitor = self.rect_for_window(pos, (size.w, size.h));
                    let (new_pos, new_size) = shrunk_geometry(monitor, start_data.location);
                    let restore = self.desk.float_in_place_where(|w| w == window);
                    self.desk.set_pos_where(|w| w == window, new_pos);
                    configure_mode(&toplevel, WindowMode::Floating, false, Some(new_size));
                    initial_pos = new_pos;
                    back = Some((mode, restore));
                }
                WindowMode::Snapped(_) => {
                    let restore = self.desk.float_in_place_where(|w| w == window);
                    let floating = restore.map_or((size.w, size.h), |(_, size)| size);
                    initial_pos =
                        geom::pointer_anchored_pos((pos.0, pos.1, size.w, size.h), floating, at);
                    self.desk.set_pos_where(|w| w == window, initial_pos);
                    configure_mode(&toplevel, WindowMode::Floating, false, Some(floating));
                    back = Some((mode, restore));
                }
                _ => {}
            }
        }

        // Only a window that floats moves its zone: one that was maximized or snapped has just
        // been taken out of that, and is not where the zone's layout puts it.
        let floated = back.is_none();
        let mut grab = match back {
            Some((mode, restore)) => {
                MoveGrab::new_unfilled(start_data, window.clone(), initial_pos, mode, restore)
            }
            None => MoveGrab::new(start_data, window.clone(), initial_pos),
        };
        if let (Some(zone), true) = (zone, floated) {
            self.zone_drag_begin(&zone);
            grab = grab.with_zone(zone);
        }
        self.dragging_view = window.user_data().get::<ViewId>().map(|id| id.0);
        pointer.set_grab(self, grab, serial, Focus::Clear);
        true
    }

    /// Put the pointer at `pos` in the shared space, clamped to the monitors, as if the mouse
    /// had moved there: the surface under it gets the enter/motion. Returns where it ended up.
    pub fn move_cursor(&mut self, pos: (f64, f64)) -> (i32, i32) {
        let (x, y) = layout::clamp_to_rects(pos, &self.pointer_bounds());
        let pos: Point<f64, Logical> = (x, y).into();
        let under = self.pointer_focus_under(pos);
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.motion(
                self,
                under,
                &MotionEvent {
                    location: pos,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: smithay::backend::input::InputTime::now(),
                },
            );
            pointer.frame(self);
        }
        (x.round() as i32, y.round() as i32)
    }

    /// Topmost window surface under `pos`, with that surface's origin in global coordinates.
    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(Window, WlSurface, Point<f64, Logical>)> {
        // The window a toplevel drag carries is never under the pointer: it is not a drop target.
        let carried = self.toplevel_drags.dragged_window();
        for entry in self.desk.top_down().filter(|e| Some(&e.window) != carried) {
            // Same origin the renderer uses: where the window's root surface is drawn.
            let origin: Point<i32, Logical> = Point::from(entry.pos) - entry.window.geometry().loc;
            let local = pos - origin.to_f64();
            if let Some((surface, surface_loc)) =
                entry.window.surface_under(local, WindowSurfaceType::ALL)
            {
                return Some((
                    entry.window.clone(),
                    surface,
                    (origin + surface_loc).to_f64(),
                ));
            }
        }
        None
    }
}

impl JffState {
    /// Read the config file again and apply it to the running compositor. A file that can't be
    /// read or parsed leaves the config in use as it is. Runs on the `reload-config` action
    /// and when the file is saved (see `app::run`).
    pub fn reload_config(&mut self) {
        match Config::load() {
            Ok(config) => {
                let old_keyboard = self.config.keyboard().clone();
                self.config = config;
                crate::logging::apply(self.config.log());
                self.apply_keyboard_config(&old_keyboard);
                self.apply_workspace_count();
                self.reconcile_outputs();
                crate::protocol::color_management::sync_output_color(self);
                self.apply_decoration_mode();
                self.apply_hotkey_config();
                self.osd.dismiss(crate::feature::osd::Icon::Error);
                tracing::info!("config reloaded");
            }
            Err(err) => {
                tracing::warn!("config not reloaded: {err:#}");
                self.show_error("Config error", &format!("{err:#}"));
            }
        }
        self.queue_redraw_all();
    }
}

impl JffState {
    /// The hot corner settings and the monitor whose corner `p` is in, if it is in one the
    /// config turns on.
    fn hot_corner_at(&self, p: (f64, f64)) -> Option<(HotCorner, layout::Rect)> {
        let hot = self.config.hot_corner()?;
        let rects: Vec<layout::Rect> = self
            .outputs
            .iter()
            .filter(|output| hot.monitors.is_empty() || hot.monitors.contains(&output.name()))
            .map(output_rect)
            .collect();
        if !layout::inside_any_corner(p, &rects, hot.size) {
            return None;
        }
        let rect = rects
            .into_iter()
            .find(|rect| layout::contains(layout::hot_corner_rect(*rect, hot.size), p))?;
        Some((hot, rect))
    }

    /// Where the hot corner must not act: not on the TTY backend, while the session is locked,
    /// or on a monitor a fullscreen window covers. It does act while the overview is open: the
    /// corner toggles it, like the `overview` action.
    fn hot_corner_blocked(&self, monitor: layout::Rect) -> bool {
        !self.backend.is_tty()
            || self.session_locked()
            || self.fullscreen_rects().contains(&monitor)
    }

    /// Move the pointer from `from` to `to`, but not to another monitor through the edges of a
    /// hot corner: the corner can be reached by sliding along the edge without overshooting
    /// onto the neighbouring monitor. Only where the hot corner itself is active (see
    /// [`JffState::hot_corner_blocked`]), returns `to` untouched everywhere else.
    fn hot_corner_barrier(
        &self,
        from: Point<f64, Logical>,
        to: Point<f64, Logical>,
    ) -> Point<f64, Logical> {
        let (from_p, to_p) = ((from.x, from.y), (to.x, to.y));
        let all = self.output_rects();
        // Most moves stay on one monitor: nothing to do.
        if all
            .iter()
            .any(|rect| layout::contains(*rect, from_p) && layout::contains(*rect, to_p))
        {
            return to;
        }
        let Some(hot) = self.config.hot_corner() else {
            return to;
        };
        if !self.backend.is_tty() || self.session_locked() {
            return to;
        }
        let fullscreen = self.fullscreen_rects();
        let enabled: Vec<layout::Rect> = self
            .outputs
            .iter()
            .filter(|output| hot.monitors.is_empty() || hot.monitors.contains(&output.name()))
            .map(output_rect)
            .filter(|rect| !fullscreen.contains(rect))
            .collect();
        layout::hot_corner_barrier(from_p, to_p, &all, &enabled, hot.size).into()
    }

    /// Stop a running hot corner dwell, if there is one.
    pub(crate) fn cancel_hot_corner_timer(&mut self) {
        if let Some(token) = self.hot_corner_timer.take() {
            self.loop_handle.remove(token);
        }
    }

    /// The pointer is now at `pos`: start or cancel the hot corner dwell. Entering a corner
    /// starts it, leaving stops it and lets the corner fire again. Once it has fired, or an
    /// entry was blocked, the pointer has to leave the corner before it can fire again - so the
    /// overview doesn't reopen right after closing with the pointer still in the corner, and
    /// closing it takes a second visit.
    pub(crate) fn check_hot_corner(&mut self, pos: Point<f64, Logical>) {
        let Some((hot, monitor)) = self.hot_corner_at((pos.x, pos.y)) else {
            self.hot_corner_armed = true;
            self.cancel_hot_corner_timer();
            return;
        };
        // Resting in the corner already: waiting for the dwell, or it has had its turn.
        if self.hot_corner_timer.is_some() || !self.hot_corner_armed {
            return;
        }
        if self.hot_corner_blocked(monitor) {
            self.hot_corner_armed = false;
            return;
        }

        let timer = Timer::from_duration(hot.dwell);
        let inserted = self.loop_handle.insert_source(timer, |_, _, state| {
            state.hot_corner_timer = None;
            state.hot_corner_armed = false;
            state.queue_redraw_all();
            // Warps don't come through the motion path: look again before acting.
            let at = state
                .seat
                .get_pointer()
                .map(|pointer| pointer.current_location())
                .unwrap_or_default();
            if let Some((_, monitor)) = state.hot_corner_at((at.x, at.y)) {
                if !state.hot_corner_blocked(monitor) {
                    if state.overview_active() {
                        tracing::debug!("hot corner: closing the overview");
                    } else {
                        tracing::debug!("hot corner: opening the overview");
                    }
                    state.toggle_overview();
                }
            }
            TimeoutAction::Drop
        });
        match inserted {
            Ok(token) => self.hot_corner_timer = Some(token),
            Err(_) => tracing::warn!("could not start the hot corner timer"),
        }
    }
}
