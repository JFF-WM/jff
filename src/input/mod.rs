//! Input: forwards winit keyboard/pointer events to the seat, and does click-to-focus.

pub mod grabs;

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
    InputTime, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    PointerMotionEvent,
};
use smithay::desktop::{Window, WindowSurfaceType};
use smithay::input::keyboard::KeyboardSource;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, Focus, GrabStartData, MotionEvent, RelativeMotionEvent,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Point, Size, SERIAL_COUNTER};
use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraint};

use crate::config::{Action, Config};
use crate::feature::overview_mode::{overview_key, OverviewKey};
use crate::feature::screenshot::{report_failure, screenshot_key, ScreenshotKey};
use crate::input::grabs::r#move::MoveGrab;
use crate::input::grabs::resize::ResizeGrab;
use crate::primitives::geom;
use crate::primitives::layout;
use crate::protocol::layer_shell::Under;
use crate::state::JffState;

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
        match event {
            InputEvent::Keyboard { event } => {
                let Some(keyboard) = self.seat.get_keyboard() else {
                    return;
                };
                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time(&event);
                let key_code = event.key_code();

                // Step 1: update xkb state and catch compositor keybinds. Bindings come from
                // config.kdl (see config.rs). Ctrl+Alt+F1..F12 switch VT (TTY backend only).
                let pressed = event.state() == KeyState::Pressed;
                // A client that asked for the shortcuts (a game) has the keyboard: only the way
                // out of the bindings is left. Queried here, not in the closure, because the
                // keyboard is in use there.
                let inhibited = self.shortcuts_inhibited();
                let Some((bind, mods_changed)) = keyboard.input_intercept(
                    KeyboardSource::MAIN,
                    self,
                    key_code,
                    event.state(),
                    |state, mods, handle| {
                        // While the overview is open it gets first pick, and keeps releases
                        // (and every key it doesn't use) from reaching anyone.
                        let overview = state.overview_active();
                        let screenshot = state.screenshot.is_some();
                        if !pressed {
                            // A key whose press no client saw keeps its release hidden too, or
                            // they'd get a release with no press.
                            let suppressed = state.suppressed_keys.remove(&key_code);
                            return if overview || screenshot || suppressed {
                                Bind::Swallow
                            } else {
                                Bind::None
                            };
                        }
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
                                let configured =
                                    sym.and_then(|sym| state.config.action_for(mods, sym));
                                return match configured {
                                    Some(Action::Overview) => Bind::Overview(OverviewKey::Cancel),
                                    Some(Action::Quit) => Bind::Action(Action::Quit),
                                    _ => overview_key(handle.modified_sym())
                                        .map_or(Bind::Swallow, Bind::Overview),
                                };
                            }
                            if screenshot {
                                // Quitting and the overview still work (opening the overview
                                // cancels the selection). The screenshot key cancels.
                                let configured =
                                    sym.and_then(|sym| state.config.action_for(mods, sym));
                                return match configured {
                                    Some(Action::Quit) => Bind::Action(Action::Quit),
                                    Some(Action::Overview) => Bind::Action(Action::Overview),
                                    Some(Action::ScreenshotArea) => {
                                        Bind::Screenshot(ScreenshotKey::Cancel)
                                    }
                                    _ => screenshot_key(handle.modified_sym())
                                        .map_or(Bind::Swallow, Bind::Screenshot),
                                };
                            }
                            sym.and_then(|sym| state.config.action_for(mods, sym))
                                .filter(|action| {
                                    !inhibited || matches!(action, Action::ToggleShortcutsInhibit)
                                })
                                .map_or(Bind::None, |action| Bind::Action(action.clone()))
                        })();
                        // Every press kept from the client is remembered, so its release is
                        // kept too. A press that goes to the client clears the mark instead: an
                        // old one would swallow the release of a key the client sees held down,
                        // and a client repeats a held key until it is released.
                        if matches!(bind, Bind::None) {
                            if state.suppressed_keys.remove(&key_code) {
                                tracing::warn!(
                                    "key {key_code:?} was still marked as kept from clients"
                                );
                            }
                        } else {
                            state.suppressed_keys.insert(key_code);
                        }
                        bind
                    },
                ) else {
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
                keyboard.input_forward(
                    self,
                    event.key_code(),
                    event.state(),
                    serial,
                    time,
                    mods_changed,
                );
            }

            InputEvent::PointerMotionAbsolute { event } => {
                // Winit reports a position relative to the host window, no flip needed.
                let rects = self.output_rects();
                let (x, y, w, h) = layout::bounds(&rects);
                let size: Size<i32, Logical> = (w, h).into();
                let pos = event.position_transformed(size) + Point::from((x as f64, y as f64));
                let pos: Point<f64, Logical> =
                    layout::clamp_to_rects((pos.x, pos.y), &self.pointer_bounds()).into();

                let under = self.pointer_focus_under(pos);
                let pointer = self.seat.get_pointer().unwrap();
                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: pos,
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time(),
                    },
                );
                pointer.frame(self);
            }

            // Mice and touchpads (libinput) report how far the pointer moved, not where it is.
            InputEvent::PointerMotion { event } => {
                let rects = self.pointer_bounds();
                let pointer = self.seat.get_pointer().unwrap();
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
                        delta: event.delta(),
                        delta_unaccel: event.delta_unaccel(),
                        time: event.time(),
                    },
                );

                // Locked: the cursor stays put, the client only sees relative motion.
                if locked {
                    pointer.frame(self);
                    return;
                }

                let pos = cur + event.delta();
                let pos: Point<f64, Logical> =
                    layout::clamp_to_rects((pos.x, pos.y), &rects).into();
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

                pointer.motion(
                    self,
                    new_under.clone(),
                    &MotionEvent {
                        location: pos,
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time(),
                    },
                );
                pointer.frame(self);

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

            InputEvent::PointerButton { event } => {
                self.pointer_button(event.button_code(), event.state(), event.time());
            }

            InputEvent::PointerAxis { event } => {
                let pointer = self.seat.get_pointer().unwrap();
                let source = event.source();
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

            _ => {}
        }
    }

    /// A pointer button went down or up: mouse bindings, click-to-focus, window drags, the
    /// overview and screenshot selection, then the client under the pointer. Real mice and
    /// `jff msg click` both come through here.
    pub(crate) fn pointer_button(&mut self, button: u32, state: ButtonState, time: InputTime) {
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
            }
            ButtonState::Released => {
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
            Action::Close => self.close_focused(),
            Action::Overview => self.toggle_overview(),
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
            pointer.set_grab(
                self,
                MoveGrab::new(start_data, window.clone(), initial_pos),
                serial,
                Focus::Clear,
            );
            return true;
        }

        let Some(toplevel) = window.toplevel().cloned() else {
            return false;
        };
        let local = (
            location.x - initial_pos.0 as f64,
            location.y - initial_pos.1 as f64,
        );
        let edges = geom::nearest_corner(local, (size.w, size.h));
        let grab = ResizeGrab::new(start_data, toplevel, edges, initial_pos, (size.w, size.h));
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
        for entry in self.desk.top_down() {
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
                self.apply_keyboard_config(&old_keyboard);
                self.apply_workspace_count();
                self.reconcile_outputs();
                self.apply_decoration_mode();
                self.osd.dismiss(crate::feature::osd::Icon::Error);
                tracing::info!("config reloaded");
            }
            Err(err) => {
                tracing::warn!("config not reloaded: {err:#}");
                self.show_error("Config error", &format!("{err:#}"));
            }
        }
    }
}
