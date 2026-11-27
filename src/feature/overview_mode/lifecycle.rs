//! Opening, closing and retargeting the overview and the window switcher.

use super::*;

impl JffState {
    /// A window opened on the monitor `output` while the overview is up. True when the overview
    /// is on another monitor and stays open: `window` is then what the keyboard goes back to
    /// when the overview is left without picking one. False (nothing changed) for the window
    /// switcher, an overview that is closing, and a window on the overview's own monitor, which
    /// the overview closes in favour of.
    pub fn overview_stays_for(&mut self, window: &Window, output: &str) -> bool {
        let Some(ov) = self.overview.as_mut().filter(|ov| {
            ov.closing.is_none() && ov.switcher.is_none() && ov.output.name() != output
        }) else {
            return false;
        };
        ov.previous_focus = Some(window.clone());
        true
    }

    pub fn toggle_overview(&mut self) {
        if self.overview_active() {
            self.end_overview(None);
        } else {
            self.open_overview();
        }
    }

    /// `overview { auto-open }`: open the overview when the monitor in use has just run out of
    /// windows, so you can type the name of an app and start it. Called once per event-loop
    /// iteration.
    ///
    /// It acts on the moment a monitor *becomes* empty (its last window closed or left, or it
    /// switched to an empty workspace), not on being empty: Escape closes the overview and it
    /// stays closed until the next time. Only the monitor the pointer is on opens it, the
    /// overview takes the pointer and the keyboard.
    pub fn auto_open_overview(&mut self) {
        let mut seen: HashMap<String, (usize, bool)> = HashMap::new();
        let mut open_on: Option<Output> = None;
        let active = self.active_output_name();
        for output in &self.outputs {
            let name = output.name();
            let state = (
                self.desk.current_on(&name),
                self.windows_on(output_rect(output)).is_empty(),
            );
            // The first look at a monitor only records it: nothing opens at start-up or when a
            // monitor is plugged in.
            let arrived = state.1 && self.overview_auto.get(&name).is_some_and(|b| *b != state);
            if arrived && name == active {
                open_on = Some(output.clone());
            }
            seen.insert(name, state);
        }
        self.overview_auto = seen;

        let Some(output) = open_on else {
            return;
        };
        if !self.config.overview().auto_open
            || self.overview_active()
            || self.session_locked()
            || self.screenshot.is_some()
        {
            return;
        }
        self.open_overview_with(true, Some(output));
        if self.overview_active() {
            self.queue_redraw_all();
        }
    }

    /// Open the overview as the window switcher, with the next window selected (the last one
    /// with `backwards`). It ends on its own when the modifiers held now are let go.
    pub fn open_switcher(&mut self, backwards: bool) {
        if self.overview_active() {
            return;
        }
        // Nothing to switch between without windows.
        self.open_overview_with(false, None);
        if !self.overview_active() {
            return;
        }
        let held = self
            .seat
            .get_keyboard()
            .map(|k| k.modifier_state())
            .map_or((false, false, false), |m| (m.alt, m.ctrl, m.logo));
        if let Some(ov) = self.overview.as_mut() {
            ov.switcher = Some(held);
        }
        // Start with the focused window centered, so the first press already slides.
        self.switcher_retarget(true);
        // The first stop is the window that asked for attention last (what `focus-urgent`
        // would pick) when it is on this monitor and workspace, and the next one in the row
        // otherwise. From there the keys step on as usual.
        let urgent = self.newest_urgent().filter(|window| {
            self.overview
                .as_ref()
                .is_some_and(|ov| ov.visible.contains(window))
        });
        match urgent {
            Some(window) => {
                if let Some(ov) = self.overview.as_mut() {
                    ov.selected = Some(window);
                }
                self.switcher_retarget(false);
            }
            None => self.handle_overview_key(OverviewKey::Cycle { backwards }),
        }
    }

    /// Whether the switcher is open and a modifier it was opened with is no longer in `now`.
    pub fn switcher_released(&self, now: Held) -> bool {
        self.overview
            .as_ref()
            .filter(|ov| ov.closing.is_none())
            .and_then(|ov| ov.switcher)
            .is_some_and(|held| overview::modifier_released(held, now))
    }

    /// Pick the monitor: the one under the pointer, so no click is needed to choose it. With no
    /// windows on it the overview still opens there, it is where an app is started from.
    pub(super) fn overview_output(&self) -> Option<Output> {
        self.output_at_pointer()
    }

    /// Windows whose center is on `rect`, topmost first. Windows that haven't committed a buffer
    /// yet have no size and are skipped.
    pub(super) fn windows_on(&self, rect: Rect) -> Vec<Window> {
        self.desk
            .top_down()
            .filter(|entry| {
                let size = entry.window.geometry().size;
                size.w > 0
                    && size.h > 0
                    && self.rect_for_window(entry.pos, (size.w, size.h)) == rect
            })
            .map(|entry| entry.window.clone())
            .collect()
    }

    /// Open the overview. No-op while a move or resize is going on. With no windows it still
    /// opens: it is where you start an app from.
    pub fn open_overview(&mut self) {
        self.open_overview_with(true, None);
    }

    /// [`JffState::open_overview`], `allow_empty` says whether it opens without windows and
    /// `on` which monitor it opens on (`None`: the one the pointer or the focused window is on).
    pub(super) fn open_overview_with(&mut self, allow_empty: bool, on: Option<Output>) {
        if self.overview_active() {
            return;
        }
        // Still animating closed: finish that first.
        self.overview = None;

        // Overview and screenshot selection never open together - the overview wins.
        self.cancel_screenshot();

        // An open menu or popover has to go first: with a grab it holds the pointer (which
        // would block the overview opening) and without one only the client knows it's open.
        // Only the menu's own grabs are released, a move or resize keeps its pointer and still
        // stops the overview.
        self.dismiss_popups();

        if self.resize.is_some() {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if pointer.is_grabbed() {
            return; // a move or a resize has the pointer
        }
        let Some(output) = on.or_else(|| self.overview_output()) else {
            return;
        };
        let rect = output_rect(&output);
        let windows = self.windows_on(rect);
        if windows.is_empty() && !allow_empty {
            return;
        }

        // Read the installed apps again if they changed, and get the font ready: both take a
        // moment and are wanted by the time the first letter is typed.
        self.app_index.refresh();
        self.osd.preload_fonts();
        let (entries, version) = self.app_index.snapshot();

        let keyboard = self.seat.get_keyboard();
        let focused = keyboard
            .as_ref()
            .and_then(|k| k.current_focus())
            .and_then(|focus| {
                self.desk
                    .find_by(|w| w.toplevel().is_some_and(|t| *t.wl_surface() == focus))
                    .map(|entry| entry.window.clone())
            });
        let selected = focused
            .clone()
            .filter(|w| windows.contains(w))
            .or_else(|| windows.first().cloned());

        // A client may hold the pointer (pointer-constraints lock or confine): the overview
        // takes it back, before the pointer leaves the client's surface.
        self.release_pointer_constraints();

        // Bring the pointer onto the monitor first. This also tells the client under it that
        // the pointer left.
        let at = pointer.current_location();
        let (x, y) = layout::clamp_to_rects((at.x, at.y), &[rect]);
        let at: Point<f64, Logical> = (x, y).into();
        pointer.motion(
            self,
            None,
            &MotionEvent {
                location: at,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);

        let grab_serial = SERIAL_COUNTER.next_serial();
        self.overview = Some(Overview {
            output,
            visible: windows.clone(),
            windows,
            selected,
            previous_focus: focused,
            pressed: None,
            press_on_close: false,
            close_buttons: HashMap::new(),
            urgent_badges: HashMap::new(),
            press_origin: None,
            drag: None,
            layer_buttons: Vec::new(),
            grab_serial,
            opened: Instant::now(),
            animation: Duration::from_millis(self.config.overview().animation_ms.into()),
            closing: None,
            closing_from: 1.0,
            highlight: Id::new(),
            wall_ids: Vec::new(),
            cell_ids: Vec::new(),
            cell_blur_ids: Vec::new(),
            cell_labels: HashMap::new(),
            title_labels: HashMap::new(),
            wall_labels: Vec::new(),
            switcher: None,
            slide: Slide::still(0),
            badges: HashMap::new(),
            badges_for: (version, self.osd.fonts().is_some()),
            carry_badges: HashMap::new(),
            carry: Carry::new(),
            pan: None,
            stage_dim: None,
            backdrop_id: Id::new(),
            cell_fill_pixel: None,
            stage_blur_ids: Vec::new(),
            search_blur: Id::new(),
            search: Search::new(entries, version),
        });

        let start_data = GrabStartData {
            focus: None,
            button: 0,
            location: at,
        };
        pointer.set_grab(
            self,
            OverviewGrab::new(start_data),
            grab_serial,
            Focus::Clear,
        );

        // Nobody has the keyboard while the overview is open: the old focus gets a proper
        // leave, so keys held down at that moment end cleanly.
        self.update_keyboard_focus();
        // A client may have left its own cursor image behind.
        self.cursor.set_status(CursorImageStatus::default_named());
    }

    /// Leave the overview and start the closing animation. `select` is the window to focus and
    /// raise, `None` restores the focus from before opening.
    ///
    /// Must not be called from inside a method of [`OverviewGrab`] - the pointer is locked there.
    pub fn end_overview(&mut self, select: Option<Window>) {
        self.cancel_hot_corner_timer();
        // Menus raised from a panel while the overview was open go with it: a click on empty
        // space or a key ends the overview without the menu ever seeing it. Do this before
        // letting go of the pointer, so the menu's own grab isn't left behind.
        if self.overview_active() {
            self.dismiss_popups();
        }

        let Some(ov) = self.overview.as_mut() else {
            return;
        };
        if ov.closing.is_some() {
            return;
        }

        let target = select.or_else(|| ov.previous_focus.clone());
        ov.selected = target.clone().filter(|w| ov.windows.contains(w));
        // Every window travels back, not only the ones that matched.
        ov.search.query.clear();
        ov.search.apps.clear();
        ov.search.app_selected = None;
        ov.update_visible();

        ov.closing_from = ov.progress();
        ov.closing = Some(Instant::now());
        ov.drag = None;
        ov.press_origin = None;
        let grab_serial = ov.grab_serial;

        if let Some(pointer) = self.seat.get_pointer() {
            if pointer.has_grab(grab_serial) {
                pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), InputTime::now());
            }
        }
        // The window may be gone by now (cancel after it closed).
        let target = target.filter(|w| self.desk.find_by(|x| x == w).is_some());
        match target {
            Some(window) => self.focus_window(&window),
            // Nothing to go back to: keyboard goes to whatever is on top.
            None => self.refocus_topmost(),
        }
        self.refresh_pointer_focus();
        // The switcher has no closing animation.
        if self
            .overview
            .as_ref()
            .is_some_and(|ov| ov.switcher.is_some())
        {
            self.overview = None;
        }
    }

    /// Drop the overview at once, no animation (monitor unplugged, VT switch).
    pub fn abort_overview(&mut self) {
        self.cancel_hot_corner_timer();
        if self.overview.is_none() {
            return;
        }
        self.end_overview(None);
        self.overview = None;
    }

    /// Tell the client under the pointer that the pointer is there. The grab ignored what was
    /// under it, and windows may have moved since.
    pub fn refresh_pointer_focus(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let at = pointer.current_location();
        let under = self.pointer_focus_under(at);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: at,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);
    }

    /// Place of every window in the overview, without the open and close animation. In the
    /// switcher that includes where the row has slid to so far.
    pub(super) fn overview_slots(&self) -> Vec<(Window, Thumb)> {
        let Some(ov) = self.overview.as_ref() else {
            return Vec::new();
        };
        let groups = self.overview_groups();
        let mut thumbs = overview_layout(ov, self.config.switcher().thumbnail_height, &groups);
        if ov.switcher.is_some() {
            let dx = ov.slide.now().round() as i32;
            thumbs = thumbs
                .into_iter()
                .map(|t| overview::shifted(t, dx))
                .collect();
        }
        ov.visible.iter().cloned().zip(thumbs).collect()
    }

    /// Aim the switcher's row at the selected window. Does nothing while it is already headed
    /// there, otherwise it slides on from wherever it is now, or jumps with `snap`.
    pub(super) fn switcher_retarget(&mut self, snap: bool) {
        let groups = self.overview_groups();
        let Some(ov) = self.overview.as_mut().filter(|ov| ov.switcher.is_some()) else {
            return;
        };
        let settings = *self.config.switcher();
        let target = switcher_target(ov, settings.thumbnail_height, &groups);
        if snap {
            ov.slide = Slide::still(target);
        } else if ov.slide.to != target {
            ov.slide = Slide {
                from: ov.slide.now(),
                to: target,
                since: Instant::now(),
                total: Duration::from_millis(settings.slide_ms.into()),
            };
        }
    }

    /// Layer surface the pointer can reach at `p` while the overview is open: overlay and top
    /// layers are in front of the thumbnails, bottom and background are behind.
    pub fn overview_layer_under(
        &self,
        p: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.overview_dragging() {
            return None;
        }
        if let Some((_, surface, loc)) = self.layer_under(p, &ABOVE_WINDOWS) {
            return Some((surface, loc));
        }
        // A menu of a panel behind the thumbnails is in front of them.
        if let Some((_, surface, loc)) = self.layer_popup_under(p, &BELOW_WINDOWS) {
            return Some((surface, loc));
        }
        if self.overview_hit(p).is_some() {
            return None;
        }
        self.layer_under(p, &BELOW_WINDOWS)
            .map(|(_, surface, loc)| (surface, loc))
    }
}
