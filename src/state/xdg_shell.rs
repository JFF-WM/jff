//! xdg-shell: toplevels, popups, decorations and dialogs.

use super::*;

impl XdgDecorationHandler for JffState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.send_decoration_mode(&toplevel);
    }

    // Apps may ask for a mode, the config decides.
    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        self.send_decoration_mode(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.send_decoration_mode(&toplevel);
    }

    // The client dropped its decoration object, so the window is back to client-side decorations.
    fn decoration_destroyed(&mut self, _toplevel: ToplevelSurface) {}
}

// ---- xdg-shell -------------------------------------------------------------------------------

// xdg_wm_dialog_v1: a window says it is a dialog, modal or not. It is remembered until the
// window first shows, when dialogs are centred.
impl XdgDialogHandler for JffState {
    fn dialog_hint_changed(&mut self, toplevel: ToplevelSurface, hint: ToplevelDialogHint) {
        let surface = toplevel.wl_surface().clone();
        match hint {
            ToplevelDialogHint::Unknown => {
                self.dialog_toplevels.remove(&surface);
            }
            ToplevelDialogHint::Dialog | ToplevelDialogHint::Modal => {
                self.dialog_toplevels.insert(surface);
            }
        }
    }
}

impl XdgShellHandler for JffState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Let the client pick its own size, but hint how much room there is (toolkits that
        // understand bounds, like GTK4 and Qt, pick a size that fits).
        // Which monitor it opens on: the pointer's, unless a window fills that one already.
        let placement = self.placement_output_index();
        let placed_rect = placement
            .and_then(|i| self.output_rects().get(i).copied())
            .unwrap_or_else(|| self.rect_at_pointer());
        let area = (placed_rect.2, placed_rect.3);
        let (bw, bh) = geom::fit_size((i32::MAX, i32::MAX), area, WINDOW_MARGIN);
        surface.with_pending_state(|state| state.bounds = Some((bw, bh).into()));
        // The first configure waits for the app's first commit (`send_initial_configure`): an
        // app that asks for its window to be restored does so before that, and the restored
        // state has to be in the first configure.
        let window = Window::new_wayland_window(surface);
        self.assign_window_id(&window);
        // New windows open on the monitor the pointer is on, on the workspace it shows.
        let output = placement
            .and_then(|i| self.outputs.get(i))
            .map(|output| output.name())
            .unwrap_or_else(|| self.active_output_name());
        // The window the user is working in, before the new one is on the desk: with
        // `focus-new-windows` off the new window goes below it.
        let anchor = self.layout_focus();
        let cascade = self.desk.add_on(window.clone(), &output);
        let pos = (cascade.0 + placed_rect.0, cascade.1 + placed_rect.1);
        self.desk.set_pos_where(|w| *w == window, pos);
        tracing::info!("new toplevel at {pos:?}");
        // Where the keyboard was before the new window takes it: a container that has it may
        // take the window in when it shows.
        if self.config.tiling_containers() && !self.overview_active() && !self.session_locked() {
            // The window the user was working in, also while a panel or launcher menu has the
            // keyboard: that is where a window started from it comes from.
            if let Some(before) = self
                .focused_window()
                .or_else(|| self.focused_window.clone())
            {
                let root = window.toplevel().map(|t| t.wl_surface().clone());
                if let Some(root) = root {
                    self.focus_before_open.insert(root, before);
                }
            }
        }
        // New windows take focus so you can type into them straight away. With the overview
        // open that can't happen (the overview owns the keyboard), so a helper window such as
        // wl-clipboard's would wait for focus forever and block the client that spawned it
        // (nvim). Close the overview in favour of the new window instead.
        //
        // With `focus-new-windows` off the keyboard stays where it is, as long as somebody has
        // it on the monitor in use. The new window asks for attention instead.
        if !self.config.focus_new_windows() && self.keeps_focus_from_new_window(anchor.as_ref()) {
            // It opens below the window that has the keyboard, not on top of it.
            if let Some(anchor) = &anchor {
                self.desk
                    .place_below_where(|w| *w == window, |w| w == anchor);
            }
            self.mark_urgent(&window);
            tracing::debug!("new toplevel left unfocused (focus-new-windows off)");
        } else if self.overview_active() {
            // A window that opens on another monitor than the overview's (where the pointer
            // is) is no reason to close it: it opens there, as it would with no overview, and
            // takes the keyboard (`focus-new-windows`), which the overview does not hold while
            // the pointer is on that other monitor.
            if self.overview_stays_for(&window, &output) {
                self.focus_window(&window);
            } else {
                self.end_overview(Some(window.clone()));
            }
        } else {
            self.focus_window(&window);
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Save it as it was, while it is still on the desk.
        self.session_toplevel_gone(surface.wl_surface());
        // Take the last picture of the window while it can still be drawn.
        let closing = self
            .desk
            .find_by(|w| w.toplevel().map(|t| t == &surface).unwrap_or(false))
            .map(|e| (e.window.clone(), e.pos, e.workspace, e.container));
        if let Some((window, pos, workspace, _)) = &closing {
            self.start_close_fade(window, *pos, *workspace);
        }
        self.desk
            .remove_where(|w| w.toplevel().map(|t| t == &surface).unwrap_or(false));
        // The tree closed up around it: the others take its room.
        if let Some((window, _, _, Some(container))) = &closing {
            self.container_after_close(*container, window);
        }
        self.close_watch_gone(surface.wl_surface());
        self.freeze_window_gone(surface.wl_surface());
        self.dialog_toplevels.remove(surface.wl_surface());
        self.settling_dialogs.remove(surface.wl_surface());
        self.focus_before_open.remove(surface.wl_surface());
        self.toplevel_drag_gone(surface.wl_surface());
        self.zones_toplevel_gone(surface.wl_surface());
    }

    // Popups (menus, tooltips, and X11 override-redirect windows via xwayland-satellite).
    // Once tracked, Window::render_elements draws them and Window::surface_under hit-tests them.
    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        let popup = PopupKind::Xdg(surface.clone());
        self.unconstrain_popup(&popup);
        // A popup with its parent is placed by now: configure it right away. A panel's menu
        // gets its parent a moment later, `new_popup` of the layer shell configures it then.
        if find_popup_root_surface(&popup).is_ok() {
            let _ = surface.send_configure();
        }
        if let Err(err) = self.popups.track_popup(popup) {
            tracing::warn!("failed to track popup: {err:?}");
        }
    }

    // A client asked to be dragged (e.g. the user pressed on its titlebar).
    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, serial: Serial) {
        let Some(start_data) = self.interactive_start(&surface, serial) else {
            return;
        };
        let wl_surface = surface.wl_surface();
        let Some(window) = self
            .desk
            .find_by(|w| is_window_for(w, wl_surface))
            .map(|e| e.window.clone())
        else {
            return;
        };
        let draggable = matches!(
            self.desk.mode_of(|w| *w == window),
            WindowMode::Floating | WindowMode::Snapped(_)
        );
        if draggable && self.start_move(&window, start_data, serial) {
            // A finger that started this drag belongs to the grab now, not to the client.
            self.touch_taken_by_grab(serial);
        }
    }

    // A client asked to be resized from one of its edges or corners.
    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let Some(start_data) = self.interactive_start(&surface, serial) else {
            return;
        };
        let wl_surface = surface.wl_surface();
        // A member of a container moves the divider of its tree instead.
        if let Some(window) = self
            .desk
            .find_by(|w| is_window_for(w, wl_surface))
            .map(|e| e.window.clone())
        {
            let edges = crate::input::grabs::resize::edges_from(edges);
            if self.start_member_resize(&window, start_data.clone(), edges, serial) {
                self.touch_taken_by_grab(serial);
                return;
            }
        }
        if !self.desk.is_floating(|w| is_window_for(w, wl_surface)) {
            return;
        }
        let Some((initial_pos, size)) = self
            .desk
            .find_by(|w| is_window_for(w, wl_surface))
            .map(|e| (e.pos, e.window.geometry().size))
        else {
            return;
        };

        let pointer = self.seat.get_pointer().unwrap();
        let grab = ResizeGrab::new(
            start_data,
            surface,
            crate::input::grabs::resize::edges_from(edges),
            initial_pos,
            (size.w, size.h),
        );
        pointer.set_grab(self, grab, serial, Focus::Clear);
        self.touch_taken_by_grab(serial);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        tracing::info!("client requested maximize");
        self.set_maximized(&surface, true);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        tracing::info!("client requested unmaximize");
        self.set_maximized(&surface, false);
    }

    // The minimize button of a window that draws its own title bar.
    fn minimize_request(&mut self, surface: ToplevelSurface) {
        tracing::info!("client requested minimize");
        let wl_surface = surface.wl_surface();
        let window = self
            .desk
            .find_by(|w| is_window_for(w, wl_surface))
            .map(|entry| entry.window.clone());
        if let Some(window) = window {
            self.minimize_window(&window);
        }
    }

    // Games and video players ask for fullscreen: fill the whole output, no frame.
    fn fullscreen_request(&mut self, surface: ToplevelSurface, output: Option<WlOutput>) {
        tracing::info!("client requested fullscreen");
        // A client may name the monitor to cover, otherwise it is the one the window is on.
        let named = output
            .as_ref()
            .and_then(Output::from_resource)
            .filter(|output| self.outputs.contains(output));
        let monitor = named.and_then(|output| self.move_for_fullscreen(&surface, &output));
        self.set_mode_on(&surface, WindowMode::Fullscreen, monitor);
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        tracing::info!("client requested unfullscreen");
        self.set_filled(&surface, xdg_toplevel::State::Fullscreen, false);
    }

    // A popup asks to be "grabbed": it takes keyboard and pointer input until dismissed, which
    // is how menus close when you click outside them and how typing goes to an open menu.
    fn grab(&mut self, surface: PopupSurface, _seat: WlSeat, serial: Serial) {
        let popup = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        tracing::debug!(
            "popup grab requested: root={:?} window_root={} serial={serial:?}",
            root.id(),
            self.is_window_root(&root)
        );

        // A window's menu may only take the keyboard from that same window. Anything else (the
        // window isn't the one the user works in, a launcher or the overview has the keyboard)
        // would leave the grab and the keyboard focus disagreeing about who's in charge.
        let window_root = self.is_window_root(&root);
        if window_root {
            self.update_keyboard_focus();
            if self.keyboard_focus != KeyboardFocus::Window(root.clone()) {
                tracing::debug!("popup grab refused: its window does not have the keyboard");
                let _ = PopupManager::dismiss_popup(&root, &popup);
                return;
            }
        }

        let Ok(mut grab) = self
            .popups
            .grab_popup(root.clone(), popup, &self.seat, serial)
        else {
            return;
        };

        let keyboard = self.seat.get_keyboard();
        let pointer = self.seat.get_pointer();

        // Only take over when the request matches the grab already in place (the press that
        // opened the menu), a stale or unrelated request is refused.
        let kb_mismatch = keyboard.as_ref().is_some_and(|k| {
            seat_grab_is_foreign(
                k.is_grabbed(),
                k.has_grab(serial),
                grab.previous_serial().is_none_or(|s| k.has_grab(s)),
                false,
            )
        });
        // The overview's own pointer grab is not a stranger: a menu raised from a panel while
        // the overview is open takes its place, and it comes back when the menu ends.
        let overview_serial = self.overview_grab_serial();
        let ptr_mismatch = pointer.as_ref().is_some_and(|p| {
            seat_grab_is_foreign(
                p.is_grabbed(),
                p.has_grab(serial),
                grab.previous_serial().is_none_or(|s| p.has_grab(s)),
                overview_serial.is_some_and(|s| p.has_grab(s)),
            )
        });
        if kb_mismatch || ptr_mismatch {
            tracing::debug!(
                "popup grab refused: stale serial (keyboard mismatch={kb_mismatch}, pointer mismatch={ptr_mismatch})"
            );
            grab.ungrab(PopupUngrabStrategy::All);
            return;
        }

        if let Some(keyboard) = keyboard {
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = pointer {
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
        tracing::debug!(
            "popup grab taken: root={:?} window_root={window_root}",
            root.id()
        );
        self.popup_grab = Some(PopupGrabState {
            root,
            grab,
            serial,
            window_root,
        });
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&PopupKind::Xdg(surface.clone()));
        surface.send_repositioned(token);
    }
}
