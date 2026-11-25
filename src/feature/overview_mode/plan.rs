//! Working out what the overview shows this frame: the plan the render elements are made from.

use super::*;

impl JffState {
    /// What to draw for the overview this frame, if there is one. Call before borrowing the
    /// backend - the layout is recomputed every frame, there are few windows.
    pub fn overview_plan(&mut self) -> Option<OverviewPlan> {
        self.overview_tick();
        self.switcher_retarget(false);
        // The wall only exists while a thumbnail is dragged.
        let wall_cells = if self.overview_dragging() {
            self.wall_cells()
        } else {
            Vec::new()
        };
        if let Some(ov) = self.overview.as_mut() {
            while ov.wall_ids.len() < wall_cells.len() {
                ov.wall_ids.push(Id::new());
            }
        }
        let wall_labels = if wall_cells.is_empty() {
            Vec::new()
        } else {
            self.wall_labels(&wall_cells)
        };
        let wall_thumbs = self.wall_thumbs(&wall_cells);
        let wallpaper: Vec<(WlSurface, Point<i32, Logical>)> = match self.overview.as_ref() {
            Some(ov) if !wall_cells.is_empty() => {
                let map = layer_map_for_output(&ov.output);
                map.layers_on(Layer::Background)
                    .filter_map(|layer| {
                        let geo = map.layer_geometry(layer)?;
                        Some((layer.wl_surface().clone(), geo.loc))
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        let pan = self.overview.as_ref().map_or(0, Overview::pan_offset);
        let stage_wall = if self.overview_dragging() {
            None
        } else {
            self.stage_wall(pan)
        };
        self.refresh_badges();
        let carry = self.overview.as_mut().and_then(|ov| {
            let dragging = ov.drag.as_ref().map(|drag| drag.window.clone());
            ov.carry.step(dragging)
        });
        // The containers' cells. A container being carried moves as a whole, by as much as the
        // pointer moved the cell it holds it by.
        let groups = self.overview_groups();
        let cells = self.overview_cells(&groups);
        let pointer_at = self
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();
        // With a wall the dragged thumbnail is as big as one of its squares, so it can be
        // matched with one. On another monitor it is the window's real size, as it will land.
        let overview_rect = output_rect(&self.overview.as_ref()?.output);
        // (how much the dragged window is scaled, the monitor the pointer is on)
        let drag_wall = wall_cells.first().map(|cell| {
            let rects = self.output_rects();
            let here = layout::nearest((pointer_at.x, pointer_at.y), &rects)
                .map(|i| rects[i])
                .unwrap_or(overview_rect);
            let on_overview = here == overview_rect;
            let scale = if on_overview {
                f64::from(cell.2) / f64::from(overview_rect.2.max(1))
            } else {
                1.0
            };
            // Over the overview the carried thumbnail is never shorter than a wall square.
            let min_height = if on_overview { cell.3 } else { 0 };
            (scale, here, min_height)
        });
        // A container being carried moves as a whole, held by the point of its cell the pointer
        // grabbed, and with a wall it is shrunk like a window is: its cell and every window in
        // it by the same factor, so the container is as big as a square of the wall.
        // (the container, its cell in the grid, the cell as carried, the factor)
        let group_drag = self.overview.as_ref().and_then(|ov| {
            let drag = ov.drag.as_ref()?;
            let id = drag.container?;
            let (_, cell) = cells.iter().find(|(g, _)| groups[*g].id == id)?;
            let factor = match drag_wall {
                Some((scale, _, min_height)) => {
                    let mut factor = scale / cell.scale.max(f64::EPSILON);
                    let height = f64::from(cell.size.1.max(1)) * factor;
                    if height < f64::from(min_height) {
                        factor = f64::from(min_height) / f64::from(cell.size.1.max(1));
                    }
                    factor
                }
                None => 1.0,
            };
            let carried = Thumb {
                size: (
                    ((f64::from(cell.size.0) * factor).round() as i32).max(1),
                    ((f64::from(cell.size.1) * factor).round() as i32).max(1),
                ),
                scale: cell.scale * factor,
                ..*cell
            };
            let carried =
                overview::dragged_thumb(carried, drag.anchor, (pointer_at.x, pointer_at.y));
            Some((id, *cell, carried, factor))
        });
        let mut cell_plans = self.cell_plans(
            &groups,
            &cells,
            group_drag.map(|(id, _, carried, _)| (id, carried)),
        );
        for cell in &mut cell_plans {
            cell.rect.0 += pan;
            if let Some((_, at)) = cell.label.as_mut() {
                at.x += pan;
            }
        }
        let style = *self.config.overview();
        let cell_radius = style.container_radius.unwrap_or(self.config.radius() / 2);
        // Rounded cells draw their fill through the corner mask, from a pixel of its colour.
        let cell_fill_pixel = if cell_radius > 0 && style.container_background_color[3] > 0.0 {
            let [r, g, b, _] = style.container_background_color;
            let rgb = [r, g, b];
            let ov = self.overview.as_mut()?;
            if ov.cell_fill_pixel.as_ref().map(|(color, _)| *color) != Some(rgb) {
                let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
                ov.cell_fill_pixel = Some((
                    rgb,
                    MemoryRenderBuffer::from_slice(
                        &[byte(r), byte(g), byte(b), 255],
                        Fourcc::Abgr8888,
                        (1, 1),
                        1,
                        Transform::Normal,
                        None,
                    ),
                ));
            }
            ov.cell_fill_pixel
                .as_ref()
                .map(|(_, buffer)| buffer.clone())
        } else {
            None
        };
        let ov = self.overview.as_ref()?;
        let t = ov.progress();
        let at = pointer_at;
        // In the switcher a container is selected as a whole: its cell gets the outline.
        let selected_cell = if ov.switcher.is_some() {
            ov.selected
                .as_ref()
                .and_then(|window| {
                    groups
                        .iter()
                        .position(|group| group.leaves.iter().any(|(leaf, _)| leaf == window))
                })
                .and_then(|g| cells.iter().position(|(index, _)| *index == g))
                .and_then(|n| cell_plans.get(n))
                .map(|cell| cell.rect)
        } else {
            None
        };

        // While a thumbnail is carried the other thumbnails are hidden (see `overview_elements`),
        // so the squares of the containers must go too: only what is being dragged stays, the
        // carried container's own cell when a whole container is.
        if ov.drag.is_some() {
            let carried_id = group_drag.map(|(id, ..)| id);
            let mut n = 0;
            cell_plans.retain(|_| {
                let keep = cells
                    .get(n)
                    .is_some_and(|(g, _)| Some(groups[*g].id) == carried_id);
                n += 1;
                keep
            });
        }

        let thumbs: Vec<(Window, Thumb)> = self
            .overview_slots()
            .into_iter()
            .map(|(window, grid)| {
                // The animation runs between where the window really is and its grid slot.
                let real = self
                    .desk
                    .find_by(|w| *w == window)
                    .map(|entry| {
                        let size = entry.window.geometry().size;
                        Thumb {
                            pos: entry.pos,
                            size: (size.w, size.h),
                            scale: 1.0,
                        }
                    })
                    .unwrap_or(grid);
                let thumb = overview::lerp_thumb(real, grid, t);
                let thumb = match ov
                    .drag
                    .as_ref()
                    .filter(|drag| drag.window == window && drag.container.is_none())
                {
                    Some(drag) => {
                        let thumb = match drag_wall {
                            Some((scale, monitor, min_height)) => {
                                // The size the window has while it is carried, as when it is
                                // dragged by its title: a maximized one is half the monitor
                                // (it maximizes again when dropped), a snapped or fullscreen
                                // one the size it floats at, any other its own.
                                // Over another monitor nothing keeps a thumbnail from
                                // collapsing (the minimum height below is only for the overview's
                                // own monitor), so a window that fills a monitor is carried at
                                // half the monitor it is over, as a maximized one is. Its floating
                                // size is no use there: it can be tiny or none at all, and the
                                // window fills the monitor again when dropped.
                                let half = ((monitor.2 / 2).max(1), (monitor.3 / 2).max(1));
                                let elsewhere = monitor != overview_rect;
                                let carried = self.desk.find_by(|w| *w == window).map_or(
                                    real.size,
                                    |entry| match entry.mode {
                                        WindowMode::Maximized => half,
                                        WindowMode::Snapped(_) | WindowMode::Fullscreen
                                            if elsewhere =>
                                        {
                                            half
                                        }
                                        WindowMode::Snapped(_) | WindowMode::Fullscreen => {
                                            entry.restore.map_or(real.size, |(_, size)| size)
                                        }
                                        WindowMode::Floating => real.size,
                                    },
                                );
                                // The picture is the window's real buffer shrunk by `zoom`, so the
                                // thumbnail is the real size times it: another shape (a snapped
                                // window's restore size) would leave a strip of background
                                // inside the outline.
                                let wanted = (f64::from(carried.0) * scale).max(1.0);
                                let mut zoom = wanted / f64::from(real.size.0.max(1));
                                let height = f64::from(real.size.1.max(1)) * zoom;
                                if height < f64::from(min_height) {
                                    zoom = f64::from(min_height) / f64::from(real.size.1.max(1));
                                }
                                let size = (
                                    ((f64::from(real.size.0) * zoom).round() as i32).max(1),
                                    ((f64::from(real.size.1) * zoom).round() as i32).max(1),
                                );
                                Thumb {
                                    size,
                                    scale: zoom,
                                    ..thumb
                                }
                            }
                            None => thumb,
                        };
                        overview::dragged_thumb(thumb, drag.anchor, (at.x, at.y))
                    }
                    None => thumb,
                };
                // The windows of a container being carried keep their places in it, shrunk
                // with the cell.
                let thumb = match group_drag {
                    Some((id, cell, carried, factor))
                        if groups.iter().any(|group| {
                            group.id == id && group.leaves.iter().any(|(leaf, _)| *leaf == window)
                        }) =>
                    {
                        Thumb {
                            pos: (
                                carried.pos.0
                                    + (f64::from(thumb.pos.0 - cell.pos.0) * factor).round() as i32,
                                carried.pos.1
                                    + (f64::from(thumb.pos.1 - cell.pos.1) * factor).round() as i32,
                            ),
                            size: (
                                ((f64::from(thumb.size.0) * factor).round() as i32).max(1),
                                ((f64::from(thumb.size.1) * factor).round() as i32).max(1),
                            ),
                            scale: thumb.scale * factor,
                        }
                    }
                    _ => thumb,
                };
                (window, thumb)
            })
            .collect();
        // Thumbnails draw front to back in list order. While closing, the just-focused window
        // was raised in the desk, so follow the desk's real stacking: the window being selected
        // must travel in front, not behind the one that was active.
        let mut thumbs = thumbs;
        for (_, thumb) in &mut thumbs {
            thumb.pos.0 += pan;
        }
        if ov.closing.is_some() {
            let stacking: Vec<Window> = self
                .desk
                .top_down()
                .map(|entry| entry.window.clone())
                .collect();
            thumbs.sort_by_key(|(window, _)| {
                stacking
                    .iter()
                    .position(|w| w == window)
                    .unwrap_or(usize::MAX)
            });
        }
        let badge_carry = carry.and_then(|(window, mix)| {
            let index = thumbs.iter().position(|(w, _)| *w == window)?;
            Some((index, mix))
        });
        let carry_badge = badge_carry
            .and_then(|(index, _)| thumbs.get(index))
            .and_then(|(window, _)| ov.carry_badges.get(&badge_key(window)).cloned());
        let dragged = ov
            .drag
            .as_ref()
            .and_then(|drag| thumbs.iter().position(|(w, _)| *w == drag.window));
        let dragged_with: Vec<usize> = match group_drag {
            Some((id, ..)) => thumbs
                .iter()
                .enumerate()
                .filter(|(i, (window, _))| {
                    Some(*i) != dragged
                        && groups.iter().any(|group| {
                            group.id == id && group.leaves.iter().any(|(leaf, _)| leaf == window)
                        })
                })
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        };
        let selected = ov
            .selected
            .as_ref()
            .filter(|_| ov.search.app_selected.is_none())
            .and_then(|s| thumbs.iter().position(|(w, _)| w == s));

        let close = selected
            .filter(|_| ov.shows_close_button())
            .and_then(|index| {
                let rect = overview::close_rect(thumbs.get(index)?.1.rect())?;
                let (rest, hot) = ov.close_buttons.get(&rect.2)?.clone();
                Some((
                    index,
                    if layout::contains(rect, (at.x, at.y)) {
                        hot
                    } else {
                        rest
                    },
                ))
            });
        let urgent = thumbs
            .iter()
            .enumerate()
            .filter(|(_, (window, _))| window_is_urgent(window))
            .filter_map(|(index, (_, thumb))| {
                let rect = overview::urgent_rect(thumb.rect())?;
                Some((index, ov.urgent_badges.get(&rect.2)?.clone()))
            })
            .collect();
        let badges = thumbs
            .iter()
            .map(|(window, _)| ov.badges.get(&badge_key(window)).cloned())
            .collect();
        let output = ov.output.clone();
        let highlight = ov.highlight.clone();
        let ripple =
            Some(ov.opened.elapsed()).filter(|_| ov.switcher.is_none() && ov.closing.is_none());
        let radius = self.config.radius();
        let wall = (!wall_cells.is_empty())
            .then(|| WallPlan {
                ids: ov.wall_ids.clone(),
                labels: wall_labels,
                thumbs: wall_thumbs,
                wallpaper,
                current: self.desk.current_on(&output.name()),
                hover: overview::wall_hit(&wall_cells, (at.x, at.y)),
                radius: radius / 2,
                behind: false,
                dim: None,
                blur: false,
                blur_ids: Vec::new(),
                cells: wall_cells,
            })
            .or(stage_wall);
        let mut plan = OverviewPlan {
            output,
            urgent,
            thumbs,
            selected,
            dragged,
            dragged_with,
            cells: cell_plans,
            selected_cell,
            cell_alpha: t as f32,
            cell_fill: style.container_background_color,
            cell_fill_pixel,
            cell_border: (style.container_border_width, style.container_border_color),
            wall_border: (style.border_width, style.border_color),
            backdrop: style
                .backdrop_color
                .map(|color| (color, ov.backdrop_id.clone())),
            cell_radius,
            radius,
            blur_windows: self.config.blur_windows().clone(),
            blur_layers: self.config.blur_layers().clone(),
            search: self.search_plan(),
            ripple,
            close,
            badges,
            badge_alpha: t as f32,
            badge_carry,
            carry_badge,
            title: None,
            wall,
            outline: {
                let config = self.config.overview();
                (config.selection_width, config.selection_color)
            },
            highlight,
        };
        plan.title = self.selected_title(&plan);
        Some(plan)
    }

    /// The title of the selected thumbnail, painted below its icon. Only that one shows a title:
    /// not while a thumbnail is dragged, with an app of the list selected, or on a thumbnail too
    /// small to carry an icon. Painted again only when its text or size changes.
    pub(super) fn selected_title(
        &mut self,
        plan: &OverviewPlan,
    ) -> Option<(usize, MemoryRenderBuffer, (i32, i32))> {
        let fonts = self.osd.fonts()?;
        let ov = self.overview.as_mut()?;
        if ov.drag.is_some() || ov.search.app_selected.is_some() {
            return None;
        }
        let index = plan.selected?;
        let (window, thumb) = plan.thumbs.get(index)?;
        let (_, _, side, _) = overview::badge_rect(thumb.rect())?;
        let (app_id, title) = window
            .toplevel()
            .map(|toplevel| toplevel_names(toplevel.wl_surface()))
            .unwrap_or_default();
        let text = if title.is_empty() { app_id } else { title };
        if text.is_empty() {
            return None;
        }
        let px = ((f64::from(side) * 0.22).round() as i32).clamp(11, 16);
        let max_width = (thumb.size.0 - 2 * (TITLE_SIDE_GAP + TITLE_PAD_X)).max(0) as f32;
        let text = fit_text(fonts, &text, px as f32, max_width);
        if text.is_empty() {
            return None;
        }
        if ov.title_labels.len() > MAX_CELL_LABELS {
            ov.title_labels.clear();
        }
        let key = (text, px);
        if !ov.title_labels.contains_key(&key) {
            let picture = paint_title(fonts, &key.0, px as f32);
            ov.title_labels.insert(
                key.clone(),
                (buffer_of(&picture), (picture.width, picture.height)),
            );
        }
        let (buffer, size) = ov.title_labels.get(&key)?;
        Some((index, buffer.clone(), *size))
    }

    /// Paint the app icon badge of every app with a window in the overview that has none yet.
    /// The icon is the one of the installed app the window's app id belongs to, or one named
    /// like the app id, or else a lettered badge.
    pub(super) fn refresh_badges(&mut self) {
        let fonts = self.osd.fonts();
        let Some(ov) = self.overview.as_mut() else {
            return;
        };
        if ov.close_buttons.is_empty() {
            for side in overview::CLOSE_MIN..=overview::CLOSE_MAX {
                ov.close_buttons.insert(
                    side,
                    (
                        buffer_of(&search_ui::paint_close_button(side, false)),
                        buffer_of(&search_ui::paint_close_button(side, true)),
                    ),
                );
            }
        }
        if ov.urgent_badges.is_empty() {
            for side in overview::CLOSE_MIN..=overview::CLOSE_MAX {
                ov.urgent_badges
                    .insert(side, buffer_of(&search_ui::paint_urgent_badge(side)));
            }
        }
        let stamp = (ov.search.version, fonts.is_some());
        if ov.badges_for != stamp {
            ov.badges.clear();
            ov.carry_badges.clear();
            ov.badges_for = stamp;
        }
        for window in &ov.windows {
            let key = badge_key(window);
            let (app_id, own_icon) = (key.0.as_str(), key.1 != 0);
            if (app_id.is_empty() && !own_icon) || ov.badges.contains_key(&key) {
                continue;
            }
            let entry = apps::entry_for_app_id(&ov.search.entries, app_id)
                .and_then(|i| ov.search.entries.get(i));
            // What the window says itself comes first, the app's desktop file is the guess. A
            // picture smaller than the badge only wins when there is nothing else: blown up
            // it looks worse than the desktop file's icon.
            let own = window
                .toplevel()
                .filter(|_| own_icon)
                .and_then(|toplevel| toplevel_icon::client_icon(toplevel.wl_surface()));
            let (own_sharp, own_blurry) = match own {
                Some((icon, true)) => (Some(icon), None),
                Some((icon, false)) => (None, Some(icon)),
                None => (None, None),
            };
            let icon = own_sharp
                .or_else(|| {
                    entry.and_then(|entry| crate::feature::app_icons::icon_for(&entry.icon))
                })
                .or_else(|| crate::feature::app_icons::icon_for(app_id))
                .or(own_blurry);
            let name = entry.map_or(app_id, |entry| entry.name.as_str());
            let picture = search_ui::paint_badge(fonts, icon.as_deref(), name);
            ov.carry_badges.insert(key.clone(), buffer_of(&picture));
            ov.badges.insert(key, buffer_of(&picture));
        }
    }

    /// The search box and the app list, painted again only when what they show changed.
    pub(super) fn search_plan(&mut self) -> Option<SearchPlan> {
        let translucent = self.config.overview().blur && !self.config.blur().off;
        let style = *self.config.overview();
        let colors = search_ui::Colors {
            background: style.search_background_color,
            font: style.search_font_color,
        };
        let ov = self.overview.as_mut().filter(|ov| ov.switcher.is_none())?;
        let alpha = ov.progress() as f32;
        let layout = search_layout(ov);
        ov.search.list_rect = layout.list_rect;
        // `None` while the font loads: the shapes are drawn now and the text follows.
        let fonts = self.osd.fonts();
        let has_fonts = fonts.is_some();

        let (x, y, width, _) = layout.box_rect;
        let key = hash_of((
            ov.search.query.as_str(),
            width,
            has_fonts,
            translucent,
            colors.key(),
        ));
        if ov.search.box_paint.as_ref().map(|(k, _)| *k) != Some(key) {
            let picture =
                search_ui::paint_box(fonts, ov.search.query.as_str(), width, translucent, colors);
            ov.search.box_paint = Some((key, buffer_of(&picture)));
        }
        let box_buffer = ov.search.box_paint.as_ref().map(|(_, b)| b.clone())?;

        let list = match layout.list_rect {
            Some((lx, ly, lw, _)) => {
                let rows: Vec<Row> = ov
                    .search
                    .apps
                    .iter()
                    .filter_map(|&i| ov.search.entries.get(i))
                    .map(|entry| Row {
                        name: entry.name.clone(),
                        detail: entry.generic_name.clone(),
                        icon: crate::feature::app_icons::icon_for(&entry.icon),
                    })
                    .collect();
                let selected = ov.search.app_selected;
                let key = hash_of((&rows, selected, lw, has_fonts, colors.key()));
                if ov.search.list_paint.as_ref().map(|(k, _)| *k) != Some(key) {
                    let picture = search_ui::paint_list(fonts, &rows, selected, lw, colors);
                    ov.search.list_paint = Some((key, buffer_of(&picture)));
                }
                ov.search
                    .list_paint
                    .as_ref()
                    .map(|(_, b)| (b.clone(), Point::from((lx - SHADOW, ly - SHADOW))))
            }
            None => None,
        };
        Some(SearchPlan {
            box_buffer,
            box_pos: Point::from((x - SHADOW, y - SHADOW)),
            list,
            box_blur: translucent.then(|| (ov.search_blur.clone(), layout.box_rect)),
            alpha,
        })
    }
}
