//! The small Wayland handler traits `JffState` implements: buffers, seats, selections,
//! activation, pointer constraints and so on.

use super::*;

// ---- wl_compositor / wl_shm / wl_buffer / wl_output -----------------------------------------

impl CompositorHandler for JffState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        // A commit with a dmabuf is held back until the GPU has finished the client's rendering,
        // then the compositor state is told to go on. Without that the frame that samples the
        // buffer waits on the client's fence on the GPU, and the compositor waits on that frame:
        // a client that is stopped or stuck mid-render would freeze everything.
        add_pre_commit_hook::<Self, _>(surface, |state, _dh, surface| {
            let mut acquire_point = None;
            let dmabuf = with_states(surface, |data| {
                acquire_point.clone_from(
                    &data
                        .cached_state
                        .get::<DrmSyncobjCachedState>()
                        .pending()
                        .acquire_point,
                );
                data.cached_state
                    .get::<SurfaceAttributes>()
                    .pending()
                    .buffer
                    .as_ref()
                    .and_then(|assignment| match assignment {
                        BufferAssignment::NewBuffer(buffer) => get_dmabuf(buffer).ok().cloned(),
                        _ => None,
                    })
            });
            let Some(dmabuf) = dmabuf else {
                return;
            };
            let Some(client) = surface.client() else {
                return;
            };
            match acquire_point {
                // Explicit sync: the client says which fence to wait for.
                Some(acquire_point) => {
                    let Ok((blocker, source)) = acquire_point.generate_blocker() else {
                        return;
                    };
                    let inserted = state.loop_handle.insert_source(source, move |_, _, data| {
                        let dh = data.display_handle.clone();
                        data.client_compositor_state(&client)
                            .blocker_cleared(data, &dh);
                        Ok(())
                    });
                    if inserted.is_ok() {
                        add_blocker(surface, blocker);
                    }
                }
                // Implicit sync: the fences ride on the dmabuf itself, wait until it is readable.
                None => {
                    let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) else {
                        return;
                    };
                    let inserted = state.loop_handle.insert_source(source, move |_, _, data| {
                        let dh = data.display_handle.clone();
                        data.client_compositor_state(&client)
                            .blocker_cleared(data, &dh);
                        Ok(())
                    });
                    if inserted.is_ok() {
                        add_blocker(surface, blocker);
                    }
                }
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Imports the attached buffer so the renderer can draw it. Also reached when a barrier
        // or a blocker clears and a held commit applies, outside any client request.
        on_commit_buffer_handler::<Self>(surface);
        self.queue_redraw_all();

        // Popups need their own commit bookkeeping before they can be drawn.
        self.popups.commit(surface);
        self.send_initial_popup_configure(surface);

        // Subsurface commits belong to the toplevel at the root of the tree.
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }

        // A commit can map a subsurface or popup, or a panel: it has to hear which monitor it
        // is on, which the signature of `refresh_window_outputs` cannot see. Only that kind of
        // commit asks for the pass: a video's frames do not.
        self.note_commit_for_outputs(surface, &root);

        // The stats overlay counts the frames of the window it follows (one bool when off).
        self.stats_commit(surface, &root);

        // A window's first commit is the answer to nothing yet: now it gets its first configure.
        let first_commit = self
            .desk
            .find_by(|w| is_window_for(w, &root))
            .and_then(|entry| entry.window.toplevel().cloned())
            .filter(|toplevel| !toplevel.is_initial_configure_sent());
        if let Some(toplevel) = first_commit {
            self.send_initial_configure(&toplevel);
        }

        if let Some(entry) = self.desk.find_by(|w| is_window_for(w, &root)) {
            entry.window.on_commit();
            self.toplevel_drag_commit(&root);
            if self.zones.has_pending() && *surface == root {
                self.zones_commit(&root);
            }
            self.fit_to_output_once(&root);
            self.recenter_settling_dialog(&root);
        } else {
            // Panels, launchers, wallpapers.
            self.layer_commit(&root);
        }
    }
}

impl BufferHandler for JffState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl DrmSyncobjHandler for JffState {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.drm_syncobj_state.as_mut()
    }
}

impl DmabufHandler for JffState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // Accept only if the renderer can really import it.
        if self.backend.renderer().import_dmabuf(&dmabuf, None).is_ok() {
            let _ = notifier.successful::<JffState>();
        } else {
            notifier.failed();
        }
    }
}

impl SecurityContextHandler for JffState {
    fn context_created(
        &mut self,
        source: SecurityContextListenerSource,
        security_context: SecurityContext,
    ) {
        // Every connection on the new socket is a sandboxed client.
        let inserted = self
            .loop_handle
            .insert_source(source, move |stream, _, state| {
                let data = ClientState {
                    pidfd: crate::protocol::ext_foreign_toplevel_pidfd::peer_pidfd(&stream),
                    security_context: Some(security_context.clone()),
                    ..ClientState::default()
                };
                if let Err(err) = state.display_handle.insert_client(stream, Arc::new(data)) {
                    tracing::warn!("error inserting sandboxed client: {err}");
                }
            });
        if let Err(err) = inserted {
            tracing::warn!("error listening on a security context socket: {err}");
        }
    }
}

impl ShmHandler for JffState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl OutputHandler for JffState {}

// ---- seat ------------------------------------------------------------------------------------

// wp_cursor_shape_v1 also serves tablet tools, so the state must handle their cursors too.
impl TabletSeatHandler for JffState {
    type ToolFocus = WlSurface;

    fn tablet_tool_image(&mut self, _tool: &TabletToolDescriptor, image: CursorImageStatus) {
        self.cursor.set_status(image);
    }
}

impl SeatHandler for JffState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    // The client under the pointer asked for a cursor (an icon name, its own surface, or none).
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor.set_status(image);
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // Clipboard offers follow keyboard focus.
        let client = focused.and_then(|s| self.display_handle.get_client(s.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client.clone());
        // Primary selection follows keyboard focus too: the focused client is the one that gets
        // to paste it when the middle button is pressed.
        set_primary_focus(&self.display_handle, seat, client);
    }
}

// ---- clipboard / drag-and-drop ---------------------------------------------------------------

// Clients exchange clipboard contents directly. jff keeps a copy of what was copied, and offers
// it itself once the app that owned the clipboard is gone (see `feature/clipboard.rs`). The
// primary selection is left to the clients.
impl SelectionHandler for JffState {
    type SelectionUserData = ();

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        if matches!(ty, SelectionTarget::Clipboard) {
            self.clipboard_selection_changed(source.map(|source| source.mime_types()));
        }
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: std::os::fd::OwnedFd,
        _seat: Seat<Self>,
        _user_data: &Self::SelectionUserData,
    ) {
        // jff only ever offers the clipboard. Dropping `fd` for anything else ends the paste.
        if matches!(ty, SelectionTarget::Clipboard) {
            self.clipboard_send(&mime_type, fd);
        } else {
            tracing::debug!("clipboard: send_selection for {ty:?}, not served");
        }
    }
}

impl DataControlHandler for JffState {
    fn data_control_state(&mut self) -> &mut DataControlState {
        &mut self.data_control_state
    }
}

impl ExtDataControlHandler for JffState {
    fn data_control_state(&mut self) -> &mut ExtDataControlState {
        &mut self.ext_data_control_state
    }
}

impl PrimarySelectionHandler for JffState {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

// wp_fractional_scale_v1: a client that just asked for the scale event gets the scale of the
// monitor its window is on right away, `refresh_window_outputs` keeps it current after.
impl FractionalScaleHandler for JffState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        let scale = self.scale_of_surface(&root);
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
        });
    }
}

impl DataDeviceHandler for JffState {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

// Drag-and-drop between clients lives in `dnd.rs`.

// xdg-foreign: nothing to do, smithay tracks the exported and imported handles and sets the
// parent of the imported toplevel (which keeps `maximize-new-windows` off file dialogs).
impl XdgForeignHandler for JffState {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.xdg_foreign_state
    }
}

// xdg-activation: focus a window when its app presents a token that came from real input.
impl XdgActivationHandler for JffState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        // No serial: the app didn't act on a key press or click, so the token can ask for
        // attention but never for the focus. Apps do this on a bell or a notification.
        let Some((serial, seat)) = data.serial else {
            data.user_data.insert_if_missing(|| UrgentOnlyMarker);
            return true;
        };
        if Seat::<JffState>::from_resource(&seat).is_none() {
            return false;
        }
        // The serial must be that of a recent key press, click or touch that went to the app
        // asking: an `enter` serial (a plain hover) is no user action. Anything else is
        // treated like no serial, the token can ask for attention but never for the focus.
        if !self.input_serials.accepts(serial, data.client_id.as_ref()) {
            tracing::debug!("activation token with a serial that is no recent input of its app");
            data.user_data.insert_if_missing(|| UrgentOnlyMarker);
        }
        true
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Stale tokens (older than a few seconds) are ignored so apps can't steal focus later.
        if token_data.timestamp.elapsed().as_secs() < 10 {
            let window = self
                .desk
                .find_by(|w| is_window_for(w, &surface))
                .map(|entry| entry.window.clone());
            if let Some(window) = window {
                if token_data.user_data.get::<UrgentOnlyMarker>().is_some() {
                    // The focus stays, the window is marked instead, until it gets the keyboard.
                    // One that has it already has nothing to ask for.
                    let focused = self.focused_window().is_some_and(|w| w == window);
                    tracing::debug!(
                        "activation without an input serial, focus left alone, urgent: {}",
                        !focused
                    );
                    if !focused {
                        self.mark_urgent(&window);
                    }
                } else {
                    // The window may be on a workspace that isn't shown: go there, or the
                    // keyboard would have nowhere to follow.
                    self.show_workspace_of(&window);
                    self.focus_window(&window);
                }
            }
        }
        // A token is good for one request.
        self.xdg_activation_state.remove_token(&token);
    }
}

// xdg-toplevel-icon: nothing to do when an icon is set, the overview reads it when it paints.
impl XdgToplevelIconHandler for JffState {}

// xdg-system-bell: no sound, so it's logged and shown as a toast.
impl XdgSystemBellHandler for JffState {
    fn ring(&mut self, surface: Option<WlSurface>) {
        tracing::debug!(
            "system bell rung{}",
            if surface.is_some() {
                " for a surface"
            } else {
                ""
            }
        );
        self.show_bell(surface);
    }
}

// pointer-constraints: games lock the pointer (mouse look) or confine it to their window.
impl PointerConstraintsHandler for JffState {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // Activate right away when the pointer is already on that surface.
        if pointer.current_focus().as_ref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        let active = with_pointer_constraint(surface, pointer, |constraint| {
            constraint.is_some_and(|c| c.is_active())
        });
        if !active {
            return;
        }
        // `location` is relative to the surface, find where its window draws it.
        let origin = self.desk.all_top_down().find_map(|entry| {
            let toplevel = entry.window.toplevel()?;
            (toplevel.wl_surface() == surface)
                .then(|| Point::<i32, Logical>::from(entry.pos) - entry.window.geometry().loc)
        });
        if let Some(origin) = origin {
            pointer.set_location(origin.to_f64() + location);
        }
    }
}
