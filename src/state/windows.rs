//! Windows on the desk: window rules, placement, fitting to an output, maximize, fullscreen
//! and the mode a window is in.

use super::*;

impl JffState {
    /// Keep a popup on screen: let the client's positioner flip/slide it into the output.
    /// Clients that give an exact position and no adjustments are left as they asked.
    pub(super) fn unconstrain_popup(&self, popup: &PopupKind) {
        let PopupKind::Xdg(xdg) = popup else {
            return;
        };
        let Ok(root) = find_popup_root_surface(popup) else {
            return;
        };
        let Some((win_pos, win_size)) = self
            .desk
            .find_by(|w| is_window_for(w, &root))
            .map(|e| (e.pos, e.window.geometry().size))
        else {
            // Not a window's popup, maybe a menu of a panel.
            self.unconstrain_layer_popup(popup);
            return;
        };
        let (out_x, out_y, w, h) = self.rect_for_window(win_pos, (win_size.w, win_size.h));
        // The positioner works relative to the parent's window geometry, so express the output
        // rect in those terms: shift by the window position and the popup's offset in it.
        let offset = get_popup_toplevel_coords(popup);
        let target = Rectangle::<i32, Logical>::new(
            (out_x - win_pos.0 - offset.x, out_y - win_pos.1 - offset.y).into(),
            (w, h).into(),
        );
        xdg.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }

    /// Answer the first commit of a popup with its initial configure. By now the popup has its
    /// parent (a panel's menu gets it after creation) and was placed inside the monitor.
    pub(super) fn send_initial_popup_configure(&mut self, surface: &WlSurface) {
        let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface) else {
            return;
        };
        if popup.is_initial_configure_sent() {
            return;
        }
        if let Err(err) = popup.send_configure() {
            tracing::warn!("initial popup configure failed: {err:?}");
        }
    }

    /// Some apps open bigger than the screen (gedit asks for ~900x700). The first time a window
    /// shows real content, shrink it with a configure so its buttons and edges are reachable.
    pub(super) fn fit_to_output_once(&mut self, root: &WlSurface) {
        let Some(entry) = self.desk.find_by(|w| is_window_for(w, root)) else {
            return;
        };
        let size = entry.window.geometry().size;
        let pos = entry.pos;
        let Some(toplevel) = entry.window.toplevel().cloned() else {
            return;
        };
        if size.w <= 0 || size.h <= 0 {
            return; // no buffer yet: wait for the first real commit
        }
        if !self.desk.mark_fitted_where(|w| is_window_for(w, root)) {
            return;
        }
        // A window a drag carries (xdg-toplevel-drag) is where the pointer puts it: not
        // maximized, centred or fitted like a window that opens on its own.
        if self.toplevel_drags.is_attached(root) {
            return;
        }
        if !self.desk.is_floating(|w| is_window_for(w, root)) {
            // The rule's opacity is not about placement: a window that opens maximized has it too.
            let dialog = toplevel.parent().is_some() || self.dialog_toplevels.contains(root);
            self.apply_rule_look(root, dialog, false);
            // An app that asks to be maximized before its first buffer (GTK apps that remember
            // their last state, Nautilus among them) is not floating here, but a container with
            // the keyboard still takes it in.
            let maximized = self.desk.mode_of(|w| is_window_for(w, root)) == WindowMode::Maximized;
            tracing::debug!("first buffer of a window that is not floating: maximized={maximized}");
            if maximized && !self.zones_is_member(root) {
                let window = self
                    .desk
                    .find_by(|w| is_window_for(w, root))
                    .map(|entry| entry.window.clone());
                if let Some(window) = window {
                    // A window a `container-template` started goes to its container first.
                    if let Some((rule, name)) = self.template_claim(root) {
                        if self.open_in_named_container(&window, &rule, &name) {
                            return;
                        }
                    }
                    self.join_container_on_map(&window);
                }
            }
            return;
        }
        // A window in a zone was placed by its client: it is not maximized or centred, and the
        // size it has now is what its place is checked against.
        let zoned = self.zones_is_member(root);
        if zoned {
            self.zones_member_fitted(root);
        }
        // A dialog is a window that belongs to another one, or says it is a dialog. It keeps the
        // size it asks for (shrunk to fit) and is centred over its parent.
        // A window its app had put back (xdg-session-management) is where and how it was, not
        // maximized or centred like a new one. If it was maximized or the like, it is again.
        let restored = self.session_take_restored(root);
        let parent = toplevel.parent();
        let dialog = parent.is_some() || self.dialog_toplevels.contains(root);
        // `window-rule`: the first rule that fits. Matched here, not in `new_toplevel`: the app
        // id, the title, the tag and the dialog hint arrive after the toplevel is created, and
        // are all there by the first commit that has a buffer.
        let mut rule = self.matching_window_rule(root, dialog, restored.is_some());
        self.apply_rule_look(root, dialog, restored.is_some());
        // The zone has a monitor already.
        if let (true, Some(rule)) = (zoned, rule.as_mut()) {
            rule.output = None;
        }
        // `restore-apps`: a window of an app from the last session goes where that session had
        // it, the monitor and the workspace, before the rest is decided (a container only takes
        // in a window on its own workspace). A window the app put back itself, a dialog and one
        // a client placed in a zone are left to what places them. What the window's rule says
        // about the monitor and workspace gives way, the rest of it stays.
        if restored.is_none() && !dialog && !zoned {
            if let Some(place) = self.app_restore_place(root) {
                let target = WindowRule {
                    output: (!place.output.is_empty())
                        .then(|| crate::config::RuleOutput::Named(place.output.clone())),
                    workspace: place.workspace,
                    ..WindowRule::default()
                };
                tracing::info!(
                    "restore-apps: {:?} goes to workspace {:?} on {:?}",
                    place.app_id,
                    place.workspace,
                    place.output
                );
                self.apply_rule_monitor_and_workspace(root, &target);
                if let Some(rule) = rule.as_mut() {
                    rule.output = None;
                    rule.workspace = None;
                }
            }
        }
        // `tiling-containers`: whatever opens while a container has the keyboard joins its
        // layout (a restored window, one a rule keeps floating too, but not a dialog), and then the
        // container decides its place and size, not a rule or `maximize-new-windows`.
        let window = self
            .desk
            .find_by(|w| is_window_for(w, root))
            .map(|entry| entry.window.clone());
        // `open-in-container`: the rule names the container, which comes before the one that has
        // the keyboard. A dialog joins too (the docks of OBS are dialogs of its main window): a
        // rule that should not catch one says so with `match dialog=#false` or a `tag`. A
        // restored window and one a client placed in a zone are left out, and so is a window
        // when `tiling-containers` is off (it opens as usual).
        let named = match (rule.as_ref(), restored.is_none() && !zoned) {
            (Some(rule), true) => rule.container.clone().map(|name| (rule.clone(), name)),
            _ => None,
        };
        // `container-template`: a window a `spawn-container` bind started joins the template's
        // container, ahead of the container a rule names, on the workspace the bind was
        // pressed on. Not a dialog, a restored window or one a client placed in a zone.
        let named = if restored.is_none() && !dialog && !zoned {
            self.template_claim(root).or(named)
        } else {
            named
        };
        if let Some(name) = rule.as_ref().and_then(|rule| rule.container.as_deref()) {
            if named.is_none() {
                tracing::info!(
                    "open-in-container {name:?} skipped: restored={} dialog={dialog} zoned={zoned}",
                    restored.is_some()
                );
            }
        }
        if let (Some((rule, name)), Some(window)) = (named.as_ref(), window.as_ref()) {
            self.focus_before_open.remove(root);
            if self.open_in_named_container(window, rule, name) {
                return;
            }
        }
        if window.is_some_and(|window| self.join_container_on_map(&window)) {
            return;
        }
        if let Some(saved) = restored.as_ref() {
            // A restored window keeps the monitor, size and mode its session saved. A rule only
            // chooses its workspace.
            if let Some(workspace) = rule.as_ref().and_then(|rule| rule.workspace) {
                self.send_window_to_workspace(root, workspace);
            }
            if saved.mode != WindowMode::Floating {
                self.session_finish_restore(&toplevel, saved);
                return;
            }
        }
        let mut ruled_floating = false;
        if let (None, Some(rule)) = (restored.as_ref(), rule.as_ref()) {
            match self.apply_window_rule(root, &toplevel, rule, parent.as_ref(), dialog) {
                RulePlacement::Done => return,
                RulePlacement::Floating => ruled_floating = true,
                RulePlacement::Untouched => {}
            }
        }
        // The rule may have moved the window to another monitor.
        let pos = self.desk.pos_of(|w| is_window_for(w, root)).unwrap_or(pos);
        // `maximize-new-windows` (on by default). Done now rather than in `new_toplevel` so the
        // window's own size is known and remembered for when it's restored. A window a rule
        // keeps floating stays that way.
        if self.config.maximize_new_windows()
            && !dialog
            && restored.is_none()
            && !ruled_floating
            && !zoned
        {
            self.set_maximized(&toplevel, true);
            return;
        }
        if dialog && restored.is_none() && !zoned {
            self.center_dialog(root, parent.as_ref(), (size.w, size.h), None);
            return;
        }
        // Only windows that overflow the output are touched, one that exactly fills it (a game
        // asking for its screen size) must be left alone.
        let (_, _, area_w, area_h) = self.rect_for_window(pos, (size.w, size.h));
        let area = (area_w, area_h);
        if !geom::overflows((size.w, size.h), area) {
            return;
        }
        let fitted = geom::fit_size((size.w, size.h), area, WINDOW_MARGIN);
        if fitted != (size.w, size.h) {
            tracing::info!(
                "window {}x{} too big for output, fitting to {fitted:?}",
                size.w,
                size.h
            );
            toplevel.with_pending_state(|state| state.size = Some((fitted.0, fitted.1).into()));
            toplevel.send_pending_configure();
        }
    }

    /// Draw the window of `root` the way the `window-rule`s say, if they do: the opacity, how
    /// its colors are read (`sdr`, `sdr-white`), what it shows (`content-type`), `always-on-top` and `sticky`. Done once,
    /// when the window first shows: a reload does not change windows that are open.
    pub(super) fn apply_rule_look(&mut self, root: &WlSurface, dialog: bool, restored: bool) {
        if self.config.window_rules().is_empty() {
            return;
        }
        let (app_id, title) = crate::feature::osd::toplevel_names(root);
        let app_tag = crate::protocol::toplevel_tag::app_tag_of(root);
        // The tag the user's rules give the window counts here too, even when the placement
        // code has not got to it yet.
        let named = crate::protocol::toplevel_tag::user_tag_of(root).or_else(|| {
            self.config
                .user_tag_for(&WindowFacts {
                    app_id: &app_id,
                    title: &title,
                    tag: app_tag.as_deref(),
                    dialog,
                    restored,
                })
                .map(str::to_owned)
        });
        let tag = named.or(app_tag);
        let facts = WindowFacts {
            app_id: &app_id,
            title: &title,
            tag: tag.as_deref(),
            dialog,
            restored,
        };
        if let Some(opacity) = self.config.opacity_for(&facts) {
            tracing::info!("window rule opacity {opacity} for app-id={app_id:?}");
            self.desk
                .set_opacity_where(|w| is_window_for(w, root), opacity);
        }
        if self.config.always_on_top_for(&facts) {
            tracing::info!("window rule always-on-top for app-id={app_id:?}");
            self.desk.set_above_where(|w| is_window_for(w, root), true);
        }
        if self.config.sticky_for(&facts) {
            tracing::info!("window rule sticky for app-id={app_id:?}");
            self.desk.set_sticky_where(|w| is_window_for(w, root), true);
        }
        if let Some(sdr) = self.config.sdr_for(&facts) {
            tracing::info!("window rule sdr {sdr:?} for app-id={app_id:?}");
            crate::protocol::window_sdr::set(root, sdr);
        }
        if let Some(kind) = self.config.content_type_for(&facts) {
            tracing::info!("window rule content-type {kind:?} for app-id={app_id:?}");
            use crate::config::RuleContentType as Rule;
            use crate::protocol::content_type::Content;
            crate::protocol::content_type::set_override(
                root,
                Some(match kind {
                    Rule::Game => Some(Content::Game),
                    Rule::Video => Some(Content::Video),
                    Rule::Photo => Some(Content::Photo),
                    Rule::NoType => None,
                }),
            );
        }
    }

    /// The `window-rule` a window gets, `None` when there is none. `dialog` and `restored` are
    /// what the window is when it first shows (see `fit_to_output_once`).
    pub(super) fn matching_window_rule(
        &self,
        root: &WlSurface,
        dialog: bool,
        restored: bool,
    ) -> Option<WindowRule> {
        if self.config.window_rules().is_empty() {
            return None;
        }
        let (app_id, title) = crate::feature::osd::toplevel_names(root);
        // The tag the user's rules give the window, decided before the rest so that every rule
        // below (and the container and layout code later) sees it. One that was set by hand
        // already (the IPC) is kept.
        if crate::protocol::toplevel_tag::user_tag_of(root).is_none() {
            let app_tag = crate::protocol::toplevel_tag::app_tag_of(root);
            let named = self.config.user_tag_for(&WindowFacts {
                app_id: &app_id,
                title: &title,
                tag: app_tag.as_deref(),
                dialog,
                restored,
            });
            if let Some(tag) = named {
                crate::protocol::toplevel_tag::set_user_tag(root, Some(tag.to_owned()));
            }
        }
        let tag = crate::protocol::toplevel_tag::tag_of(root);
        let facts = WindowFacts {
            app_id: &app_id,
            title: &title,
            tag: tag.as_deref(),
            dialog,
            restored,
        };
        let rule = self.config.window_rule_for(&facts).cloned();
        if rule.is_some() {
            tracing::info!(
                "window rule applies: app-id={app_id:?} tag={tag:?} dialog={dialog} restored={restored}"
            );
        }
        rule
    }

    /// Send the window of `root` to workspace `workspace` (0-based, clamped).
    pub(super) fn send_window_to_workspace(&mut self, root: &WlSurface, workspace: usize) {
        let window = self
            .desk
            .find_by(|w| is_window_for(w, root))
            .map(|entry| entry.window.clone());
        if let Some(window) = window {
            self.send_to_workspace(&window, workspace);
        }
    }

    /// Move the window of `root` to the monitor and the workspace `rule` names, the window
    /// keeping the offset it had on the monitor it opened on. Returns the rectangle of the
    /// monitor it was moved to, `None` when the rule names none (or one that is not connected).
    /// Doing it again changes nothing.
    pub(crate) fn apply_rule_monitor_and_workspace(
        &mut self,
        root: &WlSurface,
        rule: &WindowRule,
    ) -> Option<layout::Rect> {
        let is_this = |w: &Window| is_window_for(w, root);
        let size = self
            .desk
            .find_by(is_this)
            .map(|entry| entry.window.geometry().size)
            .map_or((0, 0), |size| (size.w, size.h));

        // The monitor: the window moves there with the offset it had on the one it opened on.
        let mut monitor = None;
        if rule.output.is_some() {
            let rects = self.output_rects();
            let target = self.placement_output_index_for(Some(rule));
            if let Some((i, name)) = target.and_then(|i| Some((i, self.outputs.get(i)?.name()))) {
                let to = rects[i];
                let from = self
                    .desk
                    .find_by(is_this)
                    .and_then(|entry| self.outputs.iter().position(|o| o.name() == entry.output))
                    .map_or(to, |j| rects[j]);
                if let Some(pos) = self.desk.pos_of(is_this) {
                    let moved = (pos.0 - from.0 + to.0, pos.1 - from.1 + to.1);
                    self.desk
                        .set_pos_where(is_this, layout::fit_into(moved, size, to));
                }
                self.desk.set_output_where(is_this, &name);
                monitor = Some(to);
            }
        }

        // The workspace, on that monitor.
        if let Some(workspace) = rule.workspace {
            self.send_window_to_workspace(root, workspace);
        }
        monitor
    }

    /// Do what `rule` says about a window that shows for the first time and isn't restored:
    /// the monitor, then the workspace on it, then maximized or its size. What the rule leaves
    /// out stays as it is. Says what is left to do for the caller, see [`RulePlacement`].
    pub(super) fn apply_window_rule(
        &mut self,
        root: &WlSurface,
        toplevel: &ToplevelSurface,
        rule: &WindowRule,
        parent: Option<&WlSurface>,
        dialog: bool,
    ) -> RulePlacement {
        let is_this = |w: &Window| is_window_for(w, root);
        let size = self
            .desk
            .find_by(is_this)
            .map(|entry| entry.window.geometry().size)
            .map_or((0, 0), |size| (size.w, size.h));

        let monitor = self.apply_rule_monitor_and_workspace(root, rule);

        // Maximized wins over a size.
        if rule.maximized == Some(true) {
            self.set_maximized(toplevel, true);
            return RulePlacement::Done;
        }

        // The size, kept inside the monitor it is on.
        let mut sized = None;
        if let Some((width, height)) = rule.size {
            let pos = self.desk.pos_of(is_this).unwrap_or_default();
            let area = monitor.unwrap_or_else(|| self.rect_for_window(pos, size));
            let fitted = geom::fit_size((width, height), (area.2, area.3), WINDOW_MARGIN);
            toplevel.with_pending_state(|state| state.size = Some(fitted.into()));
            toplevel.send_pending_configure();
            if !dialog {
                // Still inside the monitor with the new size.
                self.desk
                    .set_pos_where(is_this, layout::fit_into(pos, fitted, area));
            }
            sized = Some(fitted);
        }

        // `open-position` and `open-centered`: where on its monitor the window starts, kept
        // inside it. A dialog goes where the rule says too, not over its parent.
        if rule.position.is_some() || rule.centered {
            let pos = self.desk.pos_of(is_this).unwrap_or_default();
            let area = monitor.unwrap_or_else(|| self.rect_for_window(pos, size));
            let win = sized.unwrap_or(size);
            let at = match rule.position {
                Some((x, y)) => (area.0 + x, area.1 + y),
                None => (area.0 + (area.2 - win.0) / 2, area.1 + (area.3 - win.1) / 2),
            };
            self.desk
                .set_pos_where(is_this, layout::fit_into(at, win, area));
            return RulePlacement::Done;
        }

        // A dialog the rule gave a monitor or a size is centred there, over its parent when
        // that is on the same monitor.
        if dialog && (sized.is_some() || monitor.is_some()) {
            self.center_dialog(root, parent, sized.unwrap_or(size), monitor);
            return RulePlacement::Done;
        }
        if sized.is_some() {
            return RulePlacement::Done;
        }
        if rule.places_window() {
            return RulePlacement::Floating;
        }
        RulePlacement::Untouched
    }

    /// Put a dialog of `size` in the middle of the window it belongs to (the monitor the pointer
    /// is on when it has none), keeping it on that monitor. One too big for the monitor is
    /// shrunk to fit first.
    ///
    /// `monitor` puts it on that monitor (a window rule asked for one) instead of the parent's,
    /// or the pointer's when it has no parent.
    pub(super) fn center_dialog(
        &mut self,
        root: &WlSurface,
        parent: Option<&WlSurface>,
        size: (i32, i32),
        monitor: Option<layout::Rect>,
    ) {
        let parent_rect = parent
            .and_then(|parent| self.desk.find_by(|w| is_window_for(w, parent)))
            .map(|entry| {
                let size = entry.window.geometry().size;
                (entry.pos.0, entry.pos.1, size.w, size.h)
            });
        let monitor = match (monitor, parent_rect) {
            (Some(monitor), _) => monitor,
            (None, Some((x, y, w, h))) => self.rect_for_window((x, y), (w, h)),
            (None, None) => self.rect_at_pointer(),
        };
        let fitted = geom::fit_size(size, (monitor.2, monitor.3), WINDOW_MARGIN);
        let pos = layout::dialog_pos(fitted, parent_rect, monitor, WINDOW_MARGIN);
        self.desk.set_pos_where(|w| is_window_for(w, root), pos);
        let since = self
            .settling_dialogs
            .get(root)
            .map_or_else(Instant::now, |settling| settling.0);
        self.settling_dialogs
            .insert(root.clone(), (since, size, pos));
        if fitted != size {
            if let Some(toplevel) = self
                .desk
                .find_by(|w| is_window_for(w, root))
                .and_then(|entry| entry.window.toplevel().cloned())
            {
                toplevel.with_pending_state(|state| state.size = Some(fitted.into()));
                toplevel.send_pending_configure();
            }
        }
    }

    /// A dialog that was centred a moment ago and has a new size now (the app showed a small
    /// window and grew it) is centred again, for its new size. Stops after a moment, or as soon
    /// as something else moved the window (the user dragging it).
    pub(super) fn recenter_settling_dialog(&mut self, root: &WlSurface) {
        let Some(&(since, last, placed)) = self.settling_dialogs.get(root) else {
            return;
        };
        let Some(entry) = self.desk.find_by(|w| is_window_for(w, root)) else {
            self.settling_dialogs.remove(root);
            return;
        };
        if since.elapsed() > DIALOG_SETTLE || entry.pos != placed {
            self.settling_dialogs.remove(root);
            return;
        }
        let size = entry.window.geometry().size;
        let size = (size.w, size.h);
        if size == last || size.0 <= 0 || size.1 <= 0 {
            return;
        }
        let parent = entry
            .window
            .toplevel()
            .and_then(|toplevel| toplevel.parent());
        self.center_dialog(root, parent.as_ref(), size, None);
    }

    pub fn set_maximized(&mut self, surface: &ToplevelSurface, on: bool) {
        self.set_filled(surface, xdg_toplevel::State::Maximized, on);
    }

    /// Maximize or fullscreen a window, or float it again. Always answers with a configure,
    /// as the protocol requires, even if nothing changed.
    pub fn set_filled(
        &mut self,
        surface: &ToplevelSurface,
        state_kind: xdg_toplevel::State,
        on: bool,
    ) {
        let wl_surface = surface.wl_surface();
        let is_this = |w: &Window| is_window_for(w, wl_surface);
        let target = match (state_kind == xdg_toplevel::State::Fullscreen, on) {
            (true, true) => WindowMode::Fullscreen,
            (true, false) => self.desk.mode_after_fullscreen(is_this),
            (false, true) => WindowMode::Maximized,
            (false, false) => WindowMode::Floating,
        };
        self.set_mode(surface, target);
    }

    /// Move a window to `output` for a fullscreen request that names it, keeping its offset on
    /// the monitor it was on. Gives the rect of `output`, `None` when the window stays where it
    /// is (a member of a container fills its container, a window that is gone).
    pub(super) fn move_for_fullscreen(
        &mut self,
        surface: &ToplevelSurface,
        output: &Output,
    ) -> Option<layout::Rect> {
        let root = surface.wl_surface();
        let is_this = |w: &Window| is_window_for(w, root);
        if self.desk.container_of(is_this).is_some() {
            return None;
        }
        let pos = self.desk.pos_of(is_this)?;
        let size = self.desk.find_by(is_this)?.window.geometry().size;
        let size = (size.w, size.h);
        let to = crate::feature::outputs::output_rect(output);
        let from = self.rect_for_window(pos, size);
        if from != to {
            let moved = (pos.0 - from.0 + to.0, pos.1 - from.1 + to.1);
            self.desk
                .set_pos_where(is_this, layout::fit_into(moved, size, to));
        }
        self.desk.set_output_where(is_this, &output.name());
        Some(to)
    }

    pub fn set_mode(&mut self, surface: &ToplevelSurface, target: WindowMode) {
        self.set_mode_on(surface, target, None);
    }

    /// [`JffState::set_mode`] on the monitor `monitor` (the window's own when `None`).
    pub(super) fn set_mode_on(
        &mut self,
        surface: &ToplevelSurface,
        target: WindowMode,
        monitor: Option<layout::Rect>,
    ) {
        let wl_surface = surface.wl_surface();
        let is_this = |w: &Window| is_window_for(w, wl_surface);
        // A member of a container is placed by its tree: maximize fills the container.
        if let Some(window) = self.desk.find_by(is_this).map(|e| e.window.clone()) {
            if self.set_member_mode(&window, target) {
                return;
            }
            // Snapping starts a container that fills the usable area (with `tiling-containers`).
            if let WindowMode::Snapped(side) = target {
                if self.snap_as_container(&window, side) {
                    return;
                }
            }
        }
        let size = self
            .desk
            .find_by(is_this)
            .map(|e| e.window.geometry().size)
            .unwrap_or_default();
        let new_size = if target == WindowMode::Floating {
            self.desk.float_where(is_this)
        } else {
            let pos = self.desk.pos_of(is_this).unwrap_or_default();
            let monitor = monitor.unwrap_or_else(|| self.rect_for_window(pos, (size.w, size.h)));
            let (x, y, w, h) = match target {
                WindowMode::Fullscreen => monitor,
                WindowMode::Snapped(side) => geom::snapped_rect(
                    self.panel_free_rect(monitor),
                    self.config.maximize_inset(),
                    self.config.snap_gap(),
                    side,
                ),
                _ => self.usable_rect(monitor),
            };
            self.desk
                .set_mode_where(is_this, target, (x, y), (size.w, size.h));
            // A fullscreen member covers its container too: on top of the other members.
            if target == WindowMode::Fullscreen && self.desk.container_of(is_this).is_some() {
                self.desk.raise_where(is_this);
            }
            Some((w, h))
        };
        let keep_maximized = self
            .desk
            .find_by(is_this)
            .is_some_and(|e| e.before_fullscreen == WindowMode::Maximized);
        tracing::info!("{target:?}, configuring size {new_size:?}");
        configure_mode(surface, target, keep_maximized, new_size);
    }

    pub fn snap_focused(&mut self, side: Side) {
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return;
        };
        let Some(toplevel) = self
            .desk
            .find_by(|w| is_window_for(w, &focus))
            .and_then(|e| e.window.toplevel().cloned())
        else {
            return;
        };
        self.set_mode(&toplevel, WindowMode::Snapped(side));
    }

    /// Toggle maximize on the window that has keyboard focus.
    pub fn toggle_maximize_focused(&mut self) {
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return;
        };
        let Some(toplevel) = self
            .desk
            .find_by(|w| is_window_for(w, &focus))
            .and_then(|e| e.window.toplevel().cloned())
        else {
            return;
        };
        let maximized = self.desk.is_maximized(|w| is_window_for(w, &focus));
        self.set_maximized(&toplevel, !maximized);
    }

    /// Toggle fullscreen on the window that has keyboard focus.
    pub fn toggle_fullscreen_focused(&mut self) {
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return;
        };
        let Some(toplevel) = self
            .desk
            .find_by(|w| is_window_for(w, &focus))
            .and_then(|e| e.window.toplevel().cloned())
        else {
            return;
        };
        let fullscreen =
            toplevel.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Fullscreen));
        self.set_filled(&toplevel, xdg_toplevel::State::Fullscreen, !fullscreen);
    }

    /// Ask the window that has keyboard focus to close itself.
    pub fn close_focused(&mut self) {
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return;
        };
        let toplevel = self
            .desk
            .find_by(|w| is_window_for(w, &focus))
            .and_then(|e| e.window.toplevel().cloned());
        if let Some(toplevel) = toplevel {
            self.request_close(&toplevel);
        }
    }

    /// Validate an interactive move/resize request. It must match the button press the client
    /// is reacting to (same serial), and that press must have landed on the client's own
    /// surface.
    pub(super) fn interactive_start(
        &self,
        surface: &ToplevelSurface,
        serial: Serial,
    ) -> Option<GrabStartData<JffState>> {
        let wl_surface = surface.wl_surface();
        let from_pointer = self.seat.get_pointer().and_then(|pointer| {
            pointer
                .with_grab(|grab_serial, grab| {
                    let data = grab.start_data();
                    let same_client = data
                        .focus
                        .as_ref()
                        .map(|(focus, _)| focus.id().same_client_as(&wl_surface.id()))
                        .unwrap_or(false);
                    (grab_serial == serial && same_client).then(|| data.clone())
                })
                .flatten()
        });
        // Not a mouse press: a touch of the client's own surface may be what it reacts to.
        from_pointer.or_else(|| self.touch_interactive_start(wl_surface, serial))
    }
}

impl JffState {
    /// The scale of the monitor `root` (a window or a layer surface) is on. First monitor
    /// stands in when it's on none, and 1.0 when there is no monitor at all.
    pub(super) fn scale_of_surface(&self, root: &WlSurface) -> f64 {
        let output = self
            .desk
            .find_by(|w| is_window_for(w, root))
            .and_then(|entry| {
                let size = entry.window.geometry().size;
                let area = (entry.pos.0, entry.pos.1, size.w.max(1), size.h.max(1));
                self.outputs.iter().find(|o| geom_intersects(area, o))
            })
            .or_else(|| {
                self.outputs.iter().find(|o| {
                    smithay::desktop::layer_map_for_output(o)
                        .layer_for_surface(root, smithay::desktop::WindowSurfaceType::TOPLEVEL)
                        .is_some()
                })
            })
            .or_else(|| self.outputs.first());
        output.map_or(1.0, |o| o.current_scale().fractional_scale())
    }
}
