//! Pointer and keyboard input while the overview or the switcher is up: hit tests, drag and
//! drop, the search box.

use super::*;

impl JffState {
    /// Whether `p` is part of the overview proper: on its monitor, clear of what panels reserve and of any panel or menu drawn over it.
    pub(super) fn overview_scroll_area(&self, p: Point<f64, Logical>) -> bool {
        let Some(ov) = self.overview.as_ref() else {
            return false;
        };
        let rect = output_rect(&ov.output);
        let zone = layer_map_for_output(&ov.output).non_exclusive_zone();
        let (x, y) = (p.x - f64::from(rect.0), p.y - f64::from(rect.1));
        let inside = x >= f64::from(zone.loc.x)
            && y >= f64::from(zone.loc.y)
            && x < f64::from(zone.loc.x + zone.size.w)
            && y < f64::from(zone.loc.y + zone.size.h);
        inside
            && self.layer_under(p, &ABOVE_WINDOWS).is_none()
            && self.layer_popup_under(p, &BELOW_WINDOWS).is_none()
    }

    /// Scroll over the overview switches workspace, one step per wheel notch or touchpad gesture. True when the event was used up here, false when it belongs to whatever is under the pointer (a panel's volume control, say).
    pub fn overview_scroll(
        &mut self,
        source: AxisSource,
        amount: f64,
        v120: Option<f64>,
        stop: bool,
    ) -> bool {
        if !self.overview_active() {
            return false;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        if !self.overview_scroll_area(pointer.current_location()) {
            self.overview_scroll.reset();
            return false;
        }
        // While typing a search the thumbnails are results, not a workspace: nothing to switch.
        if self
            .overview
            .as_ref()
            .is_some_and(|ov| !ov.search.query.is_empty())
        {
            return true;
        }
        let steps = match source {
            // One workspace per burst of notches, and none while the slide still runs.
            AxisSource::Wheel | AxisSource::WheelTilt => self.overview_scroll.wheel_paced(
                v120.unwrap_or(amount * 120.0 / 15.0),
                Instant::now(),
                PAN_DURATION,
            ),
            _ => self.overview_scroll.touch(amount, stop),
        };
        if steps == 0 {
            return true;
        }
        self.overview_navigate(steps);
        true
    }

    /// The window whose close button is under `p`: the selected one, while it shows its button.
    pub(super) fn overview_close_hit(&self, p: Point<f64, Logical>) -> Option<Window> {
        let ov = self
            .overview
            .as_ref()
            .filter(|ov| ov.shows_close_button())?;
        let selected = ov.selected.clone()?;
        let slots = self.overview_slots();
        let (_, thumb) = slots.iter().find(|(window, _)| *window == selected)?;
        let rect = overview::close_rect(thumb.rect())?;
        layout::contains(rect, (p.x, p.y)).then_some(selected)
    }

    /// The window whose thumbnail is under `p`.
    pub(super) fn overview_hit(&self, p: Point<f64, Logical>) -> Option<Window> {
        let slots = self.overview_slots();
        let thumbs: Vec<Thumb> = slots.iter().map(|(_, t)| *t).collect();
        if let Some(i) = overview::hit_test(&thumbs, (p.x, p.y)) {
            return Some(slots[i].0.clone());
        }
        // Not on a window, but in the cell of a container: the container's own window.
        let groups = self.overview_groups();
        self.overview_cells(&groups)
            .into_iter()
            .find(|(_, cell)| layout::contains(cell.rect(), (p.x, p.y)))
            .map(|(g, _)| groups[g].focused.clone())
    }

    /// Pointer moved: select what it's over. Over empty space the selection stays.
    pub fn overview_hover(&mut self, p: Point<f64, Logical>) {
        if self.overview_dragging() || self.overview_start_drag(p) {
            return;
        }
        // A panel in front of the thumbnails isn't part of the overview.
        if self.layer_under(p, &ABOVE_WINDOWS).is_some()
            || self.layer_popup_under(p, &BELOW_WINDOWS).is_some()
        {
            return;
        }
        if let Some(row) = self.overview_app_at(p) {
            if let Some(ov) = self.overview.as_mut() {
                ov.search.app_selected = Some(row);
            }
            return;
        }
        if let Some(window) = self.overview_hit(p) {
            if let Some(ov) = self.overview.as_mut() {
                ov.selected = Some(window);
                ov.search.app_selected = None;
            }
        }
    }

    /// The row of the app list under `p`.
    pub(super) fn overview_app_at(&self, p: Point<f64, Logical>) -> Option<usize> {
        let ov = self.overview.as_ref()?;
        if !ov.search.active() {
            return None;
        }
        let (x, y, w, h) = ov.search.list_rect?;
        let (px, py) = (p.x.floor() as i32, p.y.floor() as i32);
        if px < x || px >= x + w || py < y || py >= y + h {
            return None;
        }
        search_ui::tile_at(px - x, py - y, w, ov.search.apps.len())
    }

    /// Start dragging the thumbnail the left button went down on, once the pointer at `p` has
    /// moved past `DRAG_THRESHOLD`. Returns whether a drag is going on now.
    pub(super) fn overview_start_drag(&mut self, p: Point<f64, Logical>) -> bool {
        let Some((window, from)) = self
            .overview
            .as_ref()
            .and_then(|ov| ov.press_origin.clone())
        else {
            return false;
        };
        if !overview::moved_past((from.x, from.y), (p.x, p.y), DRAG_THRESHOLD) {
            return false;
        }
        let slots = self.overview_slots();
        let Some((_, thumb)) = slots.iter().find(|(w, _)| *w == window) else {
            return false;
        };
        // A window of a container takes the whole container with it, held by its cell.
        let groups = self.overview_groups();
        let held = groups
            .iter()
            .position(|group| group.leaves.iter().any(|(leaf, _)| *leaf == window))
            .and_then(|g| {
                let (_, cell) = self
                    .overview_cells(&groups)
                    .into_iter()
                    .find(|(index, _)| *index == g)?;
                Some((groups[g].id, cell))
            });
        let (anchor, container) = match held {
            Some((id, cell)) => (overview::anchor_in(&cell, (from.x, from.y)), Some(id)),
            None => (overview::anchor_in(thumb, (from.x, from.y)), None),
        };
        let Some(ov) = self.overview.as_mut() else {
            return false;
        };
        ov.press_origin = None;
        ov.selected = Some(window.clone());
        ov.drag = Some(Drag {
            window,
            anchor,
            container,
        });
        true
    }

    /// The dragged thumbnail was let go. On another monitor the window moves there, keeping its
    /// real size and the point it was held by under the pointer. On the same monitor nothing
    /// changes, the thumbnail slides back to its slot.
    pub(super) fn overview_drop(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let Some((drag, source, monitor)) = self.overview.as_mut().and_then(|ov| {
            let drag = ov.drag.take()?;
            Some((drag, output_rect(&ov.output), ov.output.name()))
        }) else {
            return;
        };
        let at = pointer.current_location();
        // Dropped on a square of the workspace wall: the window goes to that workspace, and
        // with `follow-window` (the default) that workspace is shown, with the window focused.
        let cells = self.wall_cells();
        if let Some(workspace) = overview::wall_hit(&cells, (at.x, at.y)) {
            self.send_to_workspace(&drag.window, workspace);
            if self
                .config
                .workspace_wall()
                .is_some_and(|wall| wall.follow_window)
            {
                // This closes the overview, on the monitor it was opened on.
                self.switch_workspace_on(&monitor, workspace);
                self.focus_window(&drag.window);
            }
            return;
        }
        let rects = self.output_rects();
        let target = layout::nearest((at.x, at.y), &rects)
            .map(|i| rects[i])
            .filter(|target| *target != source);
        // Over a container of the other monitor its windows join that one, as a single window
        // does, anywhere else on it the container lands whole.
        if let (Some(target), Some(container)) = (target, drag.container) {
            if !self.drop_container_into(container, at) {
                self.drop_container(container, target);
            }
            return;
        }
        let moving = self
            .desk
            .find_by(|w| *w == drag.window)
            .map(|entry| (entry.window.geometry().size, entry.restore));

        if let (Some(target), Some((size, restore))) = (target, moving) {
            let window = drag.window;
            let pos = if restore.is_some() {
                // Maximized / fullscreen windows get fitted to the monitor right after, they
                // only need to end up on the right one.
                (
                    target.0 + (target.2 - size.w) / 2,
                    target.1 + (target.3 - size.h) / 2,
                )
            } else {
                overview::drop_pos(drag.anchor, (at.x, at.y), (size.w, size.h), target)
            };
            let before = self.desk.pos_of(|w| *w == window);
            self.desk.set_pos_where(|w| *w == window, pos);
            self.desk.raise_where(|w| *w == window);
            // A floating member of a zone takes its zone along.
            if let (None, Some(before)) = (restore, before) {
                self.zones_carry(&window, before);
            }
            if let Some((restore_pos, restore_size)) = restore {
                let carried = (
                    restore_pos.0 + target.0 - source.0,
                    restore_pos.1 + target.1 - source.1,
                );
                self.desk.set_restore_pos_where(
                    |w| *w == window,
                    layout::fit_into(carried, restore_size, target),
                );
                self.relayout_outputs();
            }
            // Let go over a container of the other monitor: the window joins it, the same as
            // after dragging it there on the desktop, and does not just float on top. Does
            // nothing (and returns false) when there is no container under the pointer, the
            // window is in a container already or is snapped/fullscreen, or the setting is off.
            self.drop_into_container(&window, at);
        }
    }

    /// Button event while the overview is open. A click counts on release, so nothing of it is
    /// left over for the window that gets focused: left selects, middle closes, right sends the
    /// window to the next workspace (the last wraps around to the first), any button on empty
    /// space (or on another monitor) cancels. Left-drag on a thumbnail drags it, dropped on
    /// another monitor, the window moves there. The selected thumbnail has a close button in
    /// its top right corner: left click on it closes the window.
    pub fn overview_button(&mut self, button: u32, state: ButtonState, time: InputTime) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        // Clicks on panels and layer surfaces go to their client, the overview keeps its
        // clicks for thumbnails and empty space.
        let to_layer = match state {
            ButtonState::Pressed => self
                .overview_layer_under(pointer.current_location())
                .is_some(),
            ButtonState::Released => self.overview.as_mut().is_some_and(|ov| {
                let before = ov.layer_buttons.len();
                ov.layer_buttons.retain(|b| *b != button);
                ov.layer_buttons.len() != before
            }),
        };
        if to_layer {
            if state == ButtonState::Pressed {
                if let Some(ov) = self.overview.as_mut() {
                    ov.layer_buttons.push(button);
                }
                // The pointer may not have moved since opening: make sure the surface under it
                // has the pointer before the press. The focus is given here, not left to the
                // grab: the overview's own grab works it out itself, but a menu's grab keeps
                // only the focus it is handed, and with none the press counts as a click outside
                // the menu and closes it, so a button inside a menu could never open a submenu.
                let at = pointer.current_location();
                let focus = self.overview_layer_under(at);
                pointer.motion(
                    self,
                    focus,
                    &MotionEvent {
                        location: at,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
            }
            pointer.button(
                self,
                &ButtonEvent {
                    button,
                    state,
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
            pointer.frame(self);
            return;
        }
        match state {
            ButtonState::Pressed => {
                if self.overview_dragging() {
                    return;
                }
                let at = pointer.current_location();
                // Pressing the close button is no selection and starts no drag.
                let on_close = button == BTN_LEFT && self.overview_close_hit(at).is_some();
                let hit = if button == BTN_LEFT && !on_close {
                    self.overview_hit(at)
                } else {
                    None
                };
                if let Some(ov) = self.overview.as_mut() {
                    ov.pressed = Some(button);
                    ov.press_on_close = on_close;
                    // A press is a selection: motion may not have run since the pointer reached
                    // this thumbnail.
                    if let Some(window) = &hit {
                        ov.selected = Some(window.clone());
                        ov.search.app_selected = None;
                    }
                    ov.press_origin = hit.map(|window| (window, at));
                }
            }
            ButtonState::Released => {
                if self.overview_dragging() && button != BTN_LEFT {
                    return;
                }
                let (armed, on_close) = self.overview.as_mut().map_or((None, false), |ov| {
                    ov.press_origin = None;
                    (ov.pressed.take(), std::mem::take(&mut ov.press_on_close))
                });
                if armed != Some(button) {
                    return;
                }
                // Pressed and released on the close button: close that window.
                if button == BTN_LEFT && on_close {
                    if let Some(window) = self.overview_close_hit(pointer.current_location()) {
                        if let Some(toplevel) = window.toplevel().cloned() {
                            self.request_close(&toplevel);
                        }
                    }
                    return;
                }
                if button == BTN_LEFT && self.overview_dragging() {
                    self.overview_drop();
                    return;
                }
                // A click on another monitor leaves the overview: it closes, like on empty
                // space of its own monitor.
                let released_at = pointer.current_location();
                if self.overview.as_ref().is_some_and(|ov| {
                    !layout::contains(output_rect(&ov.output), (released_at.x, released_at.y))
                }) {
                    self.end_overview(None);
                    return;
                }
                if button == BTN_LEFT {
                    if let Some(row) = self.overview_app_at(pointer.current_location()) {
                        self.set_pick(Pick::App(row));
                        self.select_result();
                        return;
                    }
                }
                if button == BTN_LEFT {
                    if let Some(steps) = self.overview_peek_at(pointer.current_location()) {
                        self.overview_navigate(steps);
                        return;
                    }
                }
                match self.overview_hit(pointer.current_location()) {
                    Some(window) if button == BTN_LEFT => self.end_overview(Some(window)),
                    Some(window) if button == BTN_MIDDLE => {
                        if let Some(toplevel) = window.toplevel().cloned() {
                            self.request_close(&toplevel);
                        }
                    }
                    Some(window) if button == BTN_RIGHT => {
                        // On to the next workspace, the last one wraps around to the first.
                        let count = self.workspace_count().max(1);
                        if let Some(from) = self.desk.workspace_of(|w| *w == window) {
                            self.send_to_workspace(&window, (from + 1) % count);
                        }
                    }
                    Some(_) => {}
                    None => self.end_overview(None),
                }
            }
        }
    }

    /// Carry out a key pressed while the overview is open.
    pub fn handle_overview_key(&mut self, key: OverviewKey) {
        match key {
            // Escape first empties the search, then closes.
            OverviewKey::Cancel => {
                if self.overview.as_ref().is_some_and(|ov| ov.search.active()) {
                    self.edit_query(|query| {
                        query.clear();
                        true
                    });
                } else {
                    self.end_overview(None);
                }
            }
            OverviewKey::Select => self.select_result(),
            OverviewKey::Move(dir) => self.move_selection(dir),
            OverviewKey::Workspace(steps) => self.overview_navigate(steps),
            OverviewKey::Cycle { backwards } => self.cycle_selection(backwards),
            OverviewKey::Type(c) => self.edit_query(|query| query.push(c)),
            OverviewKey::Backspace => self.edit_query(Query::backspace),
        }
    }

    /// Enter: start the selected app, or leave and focus the selected window. With text typed
    /// and nothing found it does nothing.
    pub(super) fn select_result(&mut self) {
        let app = self
            .overview
            .as_ref()
            .and_then(|ov| ov.search.selected_entry());
        if let Some(entry) = app {
            self.launch_app(&entry);
            self.end_overview(None);
            return;
        }
        if self
            .overview
            .as_ref()
            .is_some_and(|ov| ov.search.active() && ov.visible.is_empty())
        {
            return;
        }
        let selected = self.overview.as_ref().and_then(|ov| ov.selected.clone());
        self.end_overview(selected);
    }

    pub(super) fn launch_app(&mut self, entry: &DesktopEntry) {
        if let Err(why) = self.app_index.launch(entry) {
            tracing::warn!("could not start {}: {why}", entry.name);
            self.show_error(&format!("Could not start {}", entry.name), &why);
        }
    }

    /// Change the search text with `edit` (which says whether it changed anything), and show
    /// the new results with the first one selected. Not in the switcher.
    pub(super) fn edit_query(&mut self, edit: impl FnOnce(&mut Query) -> bool) {
        let changed = self
            .overview
            .as_mut()
            .filter(|ov| ov.switcher.is_none() && ov.closing.is_none())
            .is_some_and(|ov| edit(&mut ov.search.query));
        if changed {
            self.refresh_search(true, true);
        }
    }

    /// Bring the results up to date: the matching windows and apps, and the selection.
    ///
    /// `reset`: select the first result (the query just changed), else only fix a selection
    /// that points at something gone. `recompute_apps`: look the apps up again (the query or
    /// the app list changed).
    pub(super) fn refresh_search(&mut self, reset: bool, recompute_apps: bool) {
        let (entries, version) = if recompute_apps {
            self.app_index.snapshot()
        } else {
            (Arc::new(Vec::new()), 0)
        };
        let Some(ov) = self.overview.as_mut() else {
            return;
        };
        if recompute_apps {
            ov.search.entries = entries;
            ov.search.version = version;
            ov.search.apps = apps::search_apps(
                &ov.search.entries,
                ov.search.query.as_str(),
                self.app_index.launches(),
                apps::MAX_RESULTS,
            );
        }
        ov.update_visible();

        if !ov.search.active() {
            ov.search.app_selected = None;
            let shown = ov.selected.as_ref().is_some_and(|s| ov.visible.contains(s));
            if !shown {
                ov.selected = ov.visible.first().cloned();
            }
            return;
        }
        let (windows, apps_len) = (ov.visible.len(), ov.search.apps.len());
        let pick = if reset {
            apps::first_pick(windows, apps_len)
        } else {
            apps::valid_pick(current_pick(ov), windows, apps_len)
        };
        apply_pick(ov, pick);
    }

    /// Select `pick`: a window among the visible ones, or an app row.
    pub(super) fn set_pick(&mut self, pick: Pick) {
        if let Some(ov) = self.overview.as_mut() {
            apply_pick(ov, Some(pick));
        }
    }

    /// Tab: the next result, over the windows and then the apps. Without a search, over the
    /// windows.
    pub(super) fn cycle_selection(&mut self, backwards: bool) {
        let search = self
            .overview
            .as_ref()
            .filter(|ov| ov.search.active())
            .map(|ov| (current_pick(ov), ov.visible.len(), ov.search.apps.len()));
        match search {
            None => {
                if !self.switcher_step(backwards) {
                    self.select_with(|thumbs, i| Some(overview::cycle(thumbs.len(), i, backwards)));
                }
            }
            Some((current, windows, apps_len)) => {
                if let Some(pick) = apps::step_pick(current, windows, apps_len, !backwards) {
                    self.set_pick(pick);
                }
            }
        }
    }

    /// In the switcher with containers shown whole: select the next stop (the previous one with
    /// `backwards`). A stop is a window that is in no container, or a container, which counts
    /// once, at the place of its most recently used window, and selects the window that is
    /// focused in it. False when this is not that, and nothing was done.
    pub(super) fn switcher_step(&mut self, backwards: bool) -> bool {
        let groups = self.overview_groups();
        if groups.is_empty() {
            return false;
        }
        let Some(ov) = self.overview.as_mut().filter(|ov| ov.switcher.is_some()) else {
            return false;
        };
        let owner = |window: &Window| {
            groups
                .iter()
                .position(|group| group.leaves.iter().any(|(leaf, _)| leaf == window))
        };
        // Each stop: the window it selects and its container, if it is one.
        let mut stops: Vec<(Window, Option<usize>)> = Vec::new();
        for window in &ov.visible {
            let group = owner(window);
            if group.is_some() && stops.iter().any(|(_, seen)| *seen == group) {
                continue;
            }
            let selects = group.map_or_else(|| window.clone(), |g| groups[g].focused.clone());
            stops.push((selects, group));
        }
        if stops.is_empty() {
            return false;
        }
        let current = ov.selected.as_ref().and_then(|selected| {
            let group = owner(selected);
            stops.iter().position(|(window, seen)| match group {
                Some(_) => *seen == group,
                None => window == selected,
            })
        });
        let next = current.map_or(0, |i| overview::cycle(stops.len(), i, backwards));
        ov.selected = Some(stops[next].0.clone());
        true
    }

    /// An arrow key. Without a search it moves between thumbnails. With one it moves between
    /// thumbnails too, and down from the bottom row goes on into the app list, where up and
    /// down walk the rows.
    pub(super) fn move_selection(&mut self, dir: Dir) {
        let searching = self.overview.as_ref().is_some_and(|ov| ov.search.active());
        if !searching {
            // In the switcher a container is one stop, left and right step over it.
            if matches!(dir, Dir::Left | Dir::Right) && self.switcher_step(dir == Dir::Left) {
                return;
            }
            self.select_with(|thumbs, i| overview::neighbor(thumbs, i, dir));
            return;
        }
        let Some((windows, apps_len, row, before)) = self.overview.as_ref().map(|ov| {
            (
                ov.visible.len(),
                ov.search.apps.len(),
                ov.search.app_selected,
                ov.selected.clone(),
            )
        }) else {
            return;
        };
        if let Some(row) = row {
            let columns = self
                .overview
                .as_ref()
                .and_then(|ov| ov.search.list_rect)
                .map_or(1, |(_, _, w, _)| search_ui::columns_for_width(w))
                as usize;
            if let Some(pick) = apps::step_in_app_grid(row, windows, apps_len, columns, dir) {
                self.set_pick(pick);
            }
            return;
        }
        self.select_with(|thumbs, i| overview::neighbor(thumbs, i, dir));
        let stayed = self.overview.as_ref().map(|ov| ov.selected.clone()) == Some(before);
        if stayed && dir == Dir::Down && apps_len > 0 {
            self.set_pick(Pick::App(0));
        }
    }

    /// Move the selection to whatever `pick` returns, given the thumbnails and the current
    /// index.
    pub(super) fn select_with(&mut self, pick: impl FnOnce(&[Thumb], usize) -> Option<usize>) {
        let slots = self.overview_slots();
        if slots.is_empty() {
            return;
        }
        let thumbs: Vec<Thumb> = slots.iter().map(|(_, t)| *t).collect();
        let current = self
            .overview
            .as_ref()
            .and_then(|ov| ov.selected.as_ref())
            .and_then(|s| slots.iter().position(|(w, _)| w == s));
        let next = match current {
            Some(i) => pick(&thumbs, i),
            None => Some(0),
        };
        if let (Some(next), Some(ov)) = (next, self.overview.as_mut()) {
            ov.selected = Some(slots[next].0.clone());
        }
    }
}
