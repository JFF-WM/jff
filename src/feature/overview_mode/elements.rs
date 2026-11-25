//! Render elements of the overview: thumbnails, badges, the workspace wall, the backdrop.

use super::*;

/// Push the app icon badge of thumbnail `index`: at the bottom centre of it, scaled with it. Call
/// before the thumbnail's own elements, which come after what is in front of them.
pub(super) fn badge_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    index: usize,
    origin: Point<i32, Logical>,
    out: &mut Vec<JffElement>,
) {
    let (Some(Some(buffer)), Some((_, thumb))) = (plan.badges.get(index), plan.thumbs.get(index))
    else {
        return;
    };
    let carried = plan.badge_carry.is_some_and(|(n, _)| n == index);
    let buffer = match (&plan.carry_badge, carried) {
        (Some(own), true) => own,
        _ => buffer,
    };
    let Some((x, y, full, _)) = overview::badge_rect(thumb.rect()) else {
        return;
    };
    // While carried the icon shrinks (to `CARRY_ICON_SCALE`) and moves to the centre.
    let mix = plan
        .badge_carry
        .filter(|(n, _)| *n == index)
        .map_or(0.0, |(_, mix)| mix);
    let side = ((f64::from(full) * (1.0 - (1.0 - CARRY_ICON_SCALE) * mix)).round() as i32).max(1);
    let x = x + (full - side) / 2;
    let centre = thumb.pos.1 + (thumb.size.1 - side) / 2;
    let y = y + (full - side) / 2;
    // With a title below it the icon sits higher, to make room for the text.
    let title = plan.title.as_ref().filter(|(n, ..)| *n == index);
    let y = match title {
        Some((_, _, (_, height))) => {
            thumb.pos.1 + thumb.size.1 - TITLE_BOTTOM_GAP - height - TITLE_ICON_GAP - side
                + (full - side) / 2
        }
        None => y,
    };
    let y = y + (f64::from(centre - y) * mix).round() as i32;
    // The picture has a shadow margin around the badge, scaled by the same factor.
    let margin = (f64::from(search_ui::BADGE_MARGIN) * f64::from(side)
        / f64::from(search_ui::BADGE_BODY))
    .round() as i32;
    // The picture is drawn at its own size and shrunk or grown by a rescale element: a scaled
    // memory element has its damage in picture pixels, so only the top-left part got redrawn.
    let corner = Point::<i32, Physical>::from((x - margin - origin.x, y - margin - origin.y));
    let zoom = f64::from(side + 2 * margin) / f64::from(search_ui::BADGE_PICTURE);
    match MemoryRenderBufferRenderElement::from_buffer(
        &mut *renderer,
        corner.to_f64(),
        buffer,
        Some(plan.badge_alpha),
        None,
        None,
        Kind::Unspecified,
    ) {
        Ok(element) => out.push(JffElement::ScaledMemory(
            RescaleRenderElement::from_element(element, corner, zoom),
        )),
        Err(err) => tracing::warn!("error drawing an app icon in the overview: {err:?}"),
    }
    // The title of the selected thumbnail: centred below the icon, near the bottom edge.
    if let Some((_, title, (width, height))) = title {
        let at: Point<f64, Physical> = (
            f64::from(thumb.pos.0 + (thumb.size.0 - width) / 2 - origin.x),
            f64::from(thumb.pos.1 + thumb.size.1 - TITLE_BOTTOM_GAP - height - origin.y),
        )
            .into();
        match MemoryRenderBufferRenderElement::from_buffer(
            &mut *renderer,
            at,
            title,
            Some(plan.badge_alpha),
            None,
            None,
            Kind::Unspecified,
        ) {
            Ok(element) => out.push(JffElement::Memory(element)),
            Err(err) => tracing::warn!("error drawing a title in the overview: {err:?}"),
        }
    }
}

/// Push the close button of thumbnail `index` if it is the selected one: in the top right corner
/// of it. Call before the thumbnail's own elements, like `badge_elements`.
pub(super) fn close_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    index: usize,
    origin: Point<i32, Logical>,
    out: &mut Vec<JffElement>,
) {
    let (Some((n, buffer)), Some((_, thumb))) = (&plan.close, plan.thumbs.get(index)) else {
        return;
    };
    if *n != index {
        return;
    }
    let Some((x, y, side, _)) = overview::close_rect(thumb.rect()) else {
        return;
    };
    let margin = search_ui::close_margin(side);
    let corner = Point::<i32, Physical>::from((x - margin - origin.x, y - margin - origin.y));
    match MemoryRenderBufferRenderElement::from_buffer(
        &mut *renderer,
        corner.to_f64(),
        buffer,
        Some(plan.badge_alpha),
        None,
        None,
        Kind::Unspecified,
    ) {
        Ok(element) => out.push(JffElement::Memory(element)),
        Err(err) => tracing::warn!("error drawing a close button in the overview: {err:?}"),
    }
}

/// Push the urgent badge of thumbnail `index` if its window asked for attention: in the top left
/// corner of it. Call before the thumbnail's own elements, like `badge_elements`.
pub(super) fn urgent_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    index: usize,
    origin: Point<i32, Logical>,
    out: &mut Vec<JffElement>,
) {
    let (Some((_, buffer)), Some((_, thumb))) = (
        plan.urgent.iter().find(|(n, _)| *n == index),
        plan.thumbs.get(index),
    ) else {
        return;
    };
    let Some((x, y, side, _)) = overview::urgent_rect(thumb.rect()) else {
        return;
    };
    let margin = search_ui::close_margin(side);
    let corner = Point::<i32, Physical>::from((x - margin - origin.x, y - margin - origin.y));
    match MemoryRenderBufferRenderElement::from_buffer(
        &mut *renderer,
        corner.to_f64(),
        buffer,
        Some(plan.badge_alpha),
        None,
        None,
        Kind::Unspecified,
    ) {
        Ok(element) => out.push(JffElement::Memory(element)),
        Err(err) => tracing::warn!("error drawing an urgent badge in the overview: {err:?}"),
    }
}

/// Push the render elements for one thumbnail, placed relative to `origin`.
pub(crate) fn thumb_elements(
    renderer: &mut GlesRenderer,
    window: &Window,
    thumb: &Thumb,
    origin: Point<i32, Logical>,
    blur: Option<config::Blur>,
    radius: i32,
    out: &mut Vec<JffElement>,
    snapshot: Option<&WallpaperSnapshot>,
) {
    let scale = Scale::from(crate::render::effects::frame_scale::get());
    // Draw the window at (0, 0) with its geometry corner, shrink it around that corner, then
    // move the result to its slot.
    let geo = window.geometry();
    let loc: Point<i32, Physical> =
        Point::<i32, Logical>::from((-geo.loc.x, -geo.loc.y)).to_physical_precise_round(scale);
    let slot: Point<i32, Physical> =
        Point::<i32, Logical>::from((thumb.pos.0 - origin.x, thumb.pos.1 - origin.y))
            .to_physical_precise_round(scale);
    // The thumbnail's corners shrink with it: its rectangle on screen and the radius times the
    // scale. Fullscreen windows are square, as they are live.
    let rounding = if is_flush(window) {
        None
    } else {
        Rounding::new(
            renderer,
            Rectangle::new(
                (thumb.pos.0 - origin.x, thumb.pos.1 - origin.y).into(),
                (thumb.size.0, thumb.size.1).into(),
            ),
            (f64::from(radius) * thumb.scale).round() as i32,
        )
    };
    // Alpha 1.0 is the thumbnail's own: a client's fade (wp_alpha_modifier_v1) is applied per
    // surface on top, so a faded window stays faded here, as it is what the user sees live.
    // A thumbnail shows the colors of the live window: converted to the monitor's like it.
    let conversions = match window.toplevel() {
        Some(toplevel) => gamut::tree_conversions(toplevel.wl_surface()),
        None => Vec::new(),
    };
    let alpha_modes = window
        .toplevel()
        .map(|toplevel| crate::render::effects::pixel_alpha::modes(toplevel.wl_surface()))
        .unwrap_or_default();
    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        window.render_elements(&mut *renderer, loc, scale, 1.0);
    for surface in surfaces {
        let conversion = gamut::find(&conversions, surface.id());
        let alpha_mode = crate::render::effects::pixel_alpha::find(&alpha_modes, surface.id());
        let shrunk = RescaleRenderElement::from_element(
            surface,
            Point::<i32, Physical>::from((0, 0)),
            thumb.scale,
        );
        let placed = RelocateRenderElement::from_element(shrunk, slot, Relocate::Relative);
        out.push(
            match gamut::masked(&mut *renderer, rounding.as_ref(), conversion, alpha_mode) {
                Some(mask) => mask.wrap_thumb(placed),
                None => JffElement::Thumb(placed),
            },
        );
    }
    // The blur goes right behind the thumbnail: after its elements in the list. The window's
    // origin sits at the slot minus its geometry offset, both shrunk with the thumbnail.
    if let (Some(blur), Some(toplevel)) = (blur, window.toplevel()) {
        let surface_loc = Point::<f64, Logical>::from((
            (thumb.pos.0 - origin.x) as f64 - geo.loc.x as f64 * thumb.scale,
            (thumb.pos.1 - origin.y) as f64 - geo.loc.y as f64 * thumb.scale,
        ));
        render_for_surface_zoomed(
            toplevel.wl_surface(),
            surface_loc,
            thumb.scale,
            blur.all_windows.then_some(geo),
            1.0,
            blur,
            rounding.as_ref(),
            &mut |element| {
                // With a snapshot the blur is of the wallpaper alone, whatever is behind.
                out.push(JffElement::from(element.with_wallpaper_source(snapshot)));
            },
        );
    }
}

/// The ripples at the monitor's top-left corner, while they play. They go in front of the layer
/// surfaces, so the renderers add them before those, right behind the cursor.
pub fn overview_ripple_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
) -> Vec<JffElement> {
    plan.ripple
        .and_then(|elapsed| ripple_element(renderer, elapsed, RIPPLE_COLOR))
        .map(JffElement::Ring)
        .into_iter()
        .collect()
}

/// Thumbnails and highlight, front to back. `origin` is the monitor's top-left in the shared
/// space, elements are placed relative to it.
///
/// `snapshot`: blur the wallpaper copy in it rather than the live frame (`None`: the live
/// frame, so windows behind a thumbnail are blurred too).
pub fn overview_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    origin: Point<i32, Logical>,
    blur: Option<config::Blur>,
    snapshot: Option<&WallpaperSnapshot>,
) -> Vec<JffElement> {
    let mut out: Vec<JffElement> = Vec::new();

    // The search box and the app list are in front of everything.
    if let Some(search) = &plan.search {
        let panels = std::iter::once((&search.box_buffer, search.box_pos))
            .chain(search.list.as_ref().map(|(buffer, pos)| (buffer, *pos)));
        for (buffer, pos) in panels {
            let loc: Point<f64, Physical> =
                (f64::from(pos.x - origin.x), f64::from(pos.y - origin.y)).into();
            match MemoryRenderBufferRenderElement::from_buffer(
                &mut *renderer,
                loc,
                buffer,
                Some(search.alpha),
                None,
                None,
                Kind::Unspecified,
            ) {
                Ok(element) => out.push(JffElement::Memory(element)),
                Err(err) => tracing::warn!("error drawing the overview search: {err:?}"),
            }
        }
        if let (Some((id, rect)), Some(blur)) = (&search.box_blur, blur) {
            let local = Rectangle::<i32, Logical>::new(
                (rect.0 - origin.x, rect.1 - origin.y).into(),
                (rect.2, rect.3).into(),
            );
            let mask = Rounding::new(renderer, local, search_ui::BOX_RADIUS);
            let element = FramebufferEffectElement::for_rect(
                id.clone(),
                local,
                BlurOptions::from(blur),
                mask,
                search.alpha.clamp(0.0, 1.0),
            );
            out.push(JffElement::from(element.with_wallpaper_source(snapshot)));
        }
    }

    // The dragged thumbnail sits in front of the rest, the workspace wall right behind it.
    // A carried container brings all its windows along, in front.
    let front: Vec<usize> = plan
        .dragged
        .into_iter()
        .chain(plan.dragged_with.iter().copied())
        .collect();
    // While a thumbnail is carried the others are hidden until it is dropped.
    let carrying = plan.dragged.is_some();
    let order = front
        .iter()
        .copied()
        .chain((0..plan.thumbs.len()).filter(|i| !carrying && !front.contains(i)));
    let behind = plan.wall.as_ref().is_some_and(|wall| wall.behind);
    let wall_at = if behind { usize::MAX } else { front.len() };
    for (n, i) in order.enumerate() {
        if n == wall_at {
            wall_elements(renderer, plan, origin, blur, snapshot, &mut out);
        }
        let (window, thumb) = &plan.thumbs[i];
        close_elements(renderer, plan, i, origin, &mut out);
        urgent_elements(renderer, plan, i, origin, &mut out);
        badge_elements(renderer, plan, i, origin, &mut out);
        thumb_elements(
            renderer,
            window,
            thumb,
            origin,
            plan.blur_windows.apply(blur, &app_id_of(window)),
            plan.radius,
            &mut out,
            snapshot,
        );
    }
    // Nothing came after the dragged thumbnail (or there are no thumbnails): the wall goes last.
    if !behind && (carrying || plan.thumbs.len() <= wall_at) {
        wall_elements(renderer, plan, origin, blur, snapshot, &mut out);
    }

    cell_elements(renderer, plan, origin, blur, snapshot, &mut out);

    // The highlight is pushed last so it's drawn behind: only its rim shows. It is a rounded
    // outline following the thumbnail's corners (`radius` shrunk with the thumbnail, fullscreen
    // windows are square), hollow so a translucent window shows no tint behind it.
    let (outline_width, outline_color) = plan.outline;
    if let Some(cell) = plan.selected_cell.filter(|_| outline_width > 0) {
        let local = Rectangle::<i32, Logical>::new(
            (cell.0 - origin.x, cell.1 - origin.y).into(),
            (cell.2, cell.3).into(),
        );
        if let Some(ring) = ring_element(
            renderer,
            local,
            outline_width,
            plan.cell_radius,
            outline_color,
        ) {
            out.push(JffElement::Ring(ring));
        }
    } else if let Some((window, thumb)) = plan
        .selected
        .filter(|i| !carrying || front.contains(i))
        .and_then(|i| plan.thumbs.get(i))
        .filter(|_| outline_width > 0)
    {
        let radius = if is_flush(window) {
            0
        } else {
            (f64::from(plan.radius) * thumb.scale).round() as i32
        };
        let thumb_rect = Rectangle::<i32, Logical>::new(
            (thumb.pos.0 - origin.x, thumb.pos.1 - origin.y).into(),
            (thumb.size.0, thumb.size.1).into(),
        );
        if let Some(ring) = ring_element(renderer, thumb_rect, outline_width, radius, outline_color)
        {
            out.push(JffElement::Ring(ring));
        } else {
            // The outline shader is unavailable: a plain rectangle behind the thumbnail.
            let geometry = Rectangle::<i32, Physical>::new(
                (
                    thumb.pos.0 - origin.x - outline_width,
                    thumb.pos.1 - origin.y - outline_width,
                )
                    .into(),
                (
                    thumb.size.0 + 2 * outline_width,
                    thumb.size.1 + 2 * outline_width,
                )
                    .into(),
            );
            let [r, g, b, a] = outline_color;
            out.push(JffElement::Solid(SolidColorRenderElement::new(
                plan.highlight.clone(),
                geometry,
                CommitCounter::default(),
                // Solid colours are premultiplied.
                [r * a, g * a, b * a, a],
                Kind::Unspecified,
            )));
        }
    }
    if behind {
        wall_elements(renderer, plan, origin, blur, snapshot, &mut out);
    }
    out
}

/// The cells of the containers: the name in front, then the outline, then the fill, all behind
/// the windows in the cell and fading in with the overview. Nothing without containers.
pub(super) fn cell_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    origin: Point<i32, Logical>,
    blur: Option<config::Blur>,
    snapshot: Option<&WallpaperSnapshot>,
    out: &mut Vec<JffElement>,
) {
    if plan.cells.is_empty() || plan.cell_alpha <= 0.01 {
        return;
    }
    let alpha = plan.cell_alpha.clamp(0.0, 1.0);
    let radius = plan.cell_radius;
    for cell in &plan.cells {
        let local = Rectangle::<i32, Logical>::new(
            (cell.rect.0 - origin.x, cell.rect.1 - origin.y).into(),
            (cell.rect.2, cell.rect.3).into(),
        );
        if let Some((buffer, pos)) = &cell.label {
            let loc: Point<f64, Physical> =
                (f64::from(pos.x - origin.x), f64::from(pos.y - origin.y)).into();
            match MemoryRenderBufferRenderElement::from_buffer(
                &mut *renderer,
                loc,
                buffer,
                Some(alpha),
                None,
                None,
                Kind::Unspecified,
            ) {
                Ok(element) => out.push(JffElement::Memory(element)),
                Err(err) => tracing::warn!("error drawing a container's name: {err:?}"),
            }
        }
        let (border_width, [r, g, b, a]) = plan.cell_border;
        if border_width > 0 {
            if let Some(ring) =
                ring_element(renderer, local, border_width, radius, [r, g, b, a * alpha])
            {
                out.push(JffElement::Ring(ring));
            }
        }
        let [r, g, b, a] = plan.cell_fill;
        let a = a * alpha;
        // A fully transparent fill adds nothing to draw, the blur behind it still shows.
        if a <= 0.0 {
        } else if let Some(pixel) = plan.cell_fill_pixel.as_ref().filter(|_| radius > 0) {
            // Rounded corners: the fill is a pixel of its colour stretched over the cell and
            // masked like the blur, so it doesn't poke out of the border's corners.
            let loc: Point<f64, Physical> = (f64::from(local.loc.x), f64::from(local.loc.y)).into();
            match MemoryRenderBufferRenderElement::from_buffer(
                &mut *renderer,
                loc,
                pixel,
                Some(a),
                None,
                Some((cell.rect.2, cell.rect.3).into()),
                Kind::Unspecified,
            ) {
                Ok(element) => out.push(match Rounding::new(renderer, local, radius) {
                    Some(mask) => mask.wrap_memory(element),
                    None => JffElement::Memory(element),
                }),
                Err(err) => tracing::warn!("error drawing a container's fill: {err:?}"),
            }
        } else {
            out.push(JffElement::Solid(SolidColorRenderElement::new(
                cell.id.clone(),
                Rectangle::<i32, Physical>::new(
                    (cell.rect.0 - origin.x, cell.rect.1 - origin.y).into(),
                    (cell.rect.2, cell.rect.3).into(),
                ),
                CommitCounter::default(),
                // Solid colours are premultiplied.
                [r * a, g * a, b * a, a],
                Kind::Unspecified,
            )));
        }
        // The blur goes right behind the fill: after it in the list, so it is drawn first and
        // the tinted fill sits on top, like frosted glass. It follows the cell's rounded
        // corners and fades with it. The windows in the cell are in front of it.
        if let Some(blur) = blur {
            let mask = Rounding::new(renderer, local, radius);
            let element = FramebufferEffectElement::for_rect(
                cell.blur_id.clone(),
                local,
                BlurOptions::from(blur),
                mask,
                alpha,
            );
            out.push(JffElement::from(element.with_wallpaper_source(snapshot)));
        }
    }
}

/// The squares of the workspace wall: the outline of the hovered one (and of the shown
/// workspace's) in front, the fills behind. Nothing without a wall in the plan.
pub(super) fn wall_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    origin: Point<i32, Logical>,
    blur: Option<config::Blur>,
    snapshot: Option<&WallpaperSnapshot>,
    out: &mut Vec<JffElement>,
) {
    let Some(wall) = &plan.wall else {
        return;
    };
    let radius = wall.radius;
    let (border_width, border_color) = plan.wall_border;
    for (i, cell) in wall.cells.iter().enumerate() {
        let local = Rectangle::<i32, Logical>::new(
            (cell.0 - origin.x, cell.1 - origin.y).into(),
            (cell.2, cell.3).into(),
        );
        let outline = if wall.hover == Some(i) {
            Some(plan.outline).filter(|(width, _)| *width > 0)
        } else if i == wall.current {
            Some((border_width, border_color.unwrap_or(WALL_CURRENT)))
        } else if wall.behind {
            // The stage: a rim around every frame, so it stands out from the blurred background.
            let strength = stage_strength(i) * plan.cell_alpha.clamp(0.0, 1.0);
            let [r, g, b, a] = border_color.unwrap_or(STAGE_BORDER);
            Some((border_width, [r, g, b, a * strength]))
        } else {
            None
        };
        if let Some((border, color)) = outline.filter(|(border, _)| *border > 0) {
            if let Some(ring) = ring_element(renderer, local, border, radius, color) {
                out.push(JffElement::Ring(ring));
            }
        }
    }
    for (i, label) in wall.labels.iter().enumerate() {
        let Some((buffer, pos)) = label else {
            continue;
        };
        // The number is for the squares with nothing in them.
        if wall.thumbs.get(i).is_some_and(|thumbs| !thumbs.is_empty()) {
            continue;
        }
        let loc: Point<f64, Physical> =
            (f64::from(pos.x - origin.x), f64::from(pos.y - origin.y)).into();
        match MemoryRenderBufferRenderElement::from_buffer(
            &mut *renderer,
            loc,
            buffer,
            Some(1.0),
            None,
            None,
            Kind::Unspecified,
        ) {
            Ok(element) => out.push(JffElement::Memory(element)),
            Err(err) => tracing::warn!("error drawing a workspace number: {err:?}"),
        }
    }
    for (window, thumb) in wall.thumbs.iter().flatten() {
        thumb_elements(
            renderer,
            window,
            thumb,
            origin,
            None,
            plan.radius,
            out,
            None,
        );
    }
    if let Some(dim) = &wall.dim {
        for cell in &wall.cells {
            let local = Rectangle::<i32, Logical>::new(
                (cell.0 - origin.x, cell.1 - origin.y).into(),
                (cell.2, cell.3).into(),
            );
            let loc: Point<f64, Physical> = (f64::from(local.loc.x), f64::from(local.loc.y)).into();
            match MemoryRenderBufferRenderElement::from_buffer(
                &mut *renderer,
                loc,
                dim,
                Some(STAGE_DIM_ALPHA * plan.cell_alpha),
                None,
                Some((cell.2, cell.3).into()),
                Kind::Unspecified,
            ) {
                Ok(element) => out.push(match Rounding::new(renderer, local, wall.radius) {
                    Some(mask) => mask.wrap_memory(element),
                    None => JffElement::Memory(element),
                }),
                Err(err) => tracing::warn!("error darkening a workspace: {err:?}"),
            }
        }
    }
    // A wall of drag squares blurs what is behind each square and shows no wallpaper in it. The
    // frames of the stage keep their wallpaper as it is and the blur goes around them instead.
    let blurred = wall.blur && blur.is_some() && !wall.behind;
    // The stage's backdrop goes behind the frames, in front of the blur and of the squares' fill.
    let mut backdrop_done = false;
    match (wall.blur, blur) {
        (true, Some(blur)) if !wall.behind => {
            for (cell, id) in wall.cells.iter().zip(&wall.blur_ids) {
                let local = Rectangle::<i32, Logical>::new(
                    (cell.0 - origin.x, cell.1 - origin.y).into(),
                    (cell.2, cell.3).into(),
                );
                let mask = Rounding::new(renderer, local, wall.radius);
                let element = FramebufferEffectElement::for_rect(
                    id.clone(),
                    local,
                    BlurOptions::from(blur),
                    mask,
                    plan.cell_alpha.clamp(0.0, 1.0),
                );
                out.push(JffElement::from(element.with_wallpaper_source(snapshot)));
            }
        }
        (true, Some(blur)) => {
            wallpaper_elements(renderer, plan, wall, origin, out);
            // A panel on the bottom layer (the windows cover it, the overview does not) is
            // drawn again here, in front of the blur, or the blur would hide it. The frames
            // still cover it where they overlap, as they do without blur.
            out.extend(layer_body_elements(
                renderer,
                &plan.output,
                &[Layer::Bottom],
                Some(blur),
                &plan.blur_layers,
            ));
            // The backdrop goes in front of the blur: a static colour hides it, one with alpha
            // tints it.
            backdrop_element(plan, origin, out);
            backdrop_done = true;
            // The whole monitor, last in the list so it is drawn first: the frames and
            // everything in them are drawn over it and stay sharp. It fades with the overview,
            // also once the frames are gone while it closes.
            if let Some(id) = wall.blur_ids.first() {
                let monitor = output_rect(&plan.output);
                let local = Rectangle::<i32, Logical>::new(
                    (monitor.0 - origin.x, monitor.1 - origin.y).into(),
                    (monitor.2, monitor.3).into(),
                );
                let element = FramebufferEffectElement::for_rect(
                    id.clone(),
                    local,
                    BlurOptions::from(blur),
                    None,
                    plan.cell_alpha.clamp(0.0, 1.0),
                );
                out.push(JffElement::from(element.with_wallpaper_source(snapshot)));
            }
        }
        _ => {
            wallpaper_elements(renderer, plan, wall, origin, out);
            if wall.behind && plan.backdrop.is_some() {
                // The backdrop would cover a panel on the bottom layer, so it is drawn again
                // in front of it, as it is with blur.
                out.extend(layer_body_elements(
                    renderer,
                    &plan.output,
                    &[Layer::Bottom],
                    blur,
                    &plan.blur_layers,
                ));
            }
        }
    }
    if wall.behind && (blurred || !wall.wallpaper.is_empty()) {
        if !backdrop_done {
            backdrop_element(plan, origin, out);
        }
        return;
    }
    for (i, cell) in wall.cells.iter().enumerate() {
        let Some(id) = wall.ids.get(i) else {
            continue;
        };
        let geometry = Rectangle::<i32, Physical>::new(
            (cell.0 - origin.x, cell.1 - origin.y).into(),
            (cell.2, cell.3).into(),
        );
        out.push(JffElement::Solid(SolidColorRenderElement::new(
            id.clone(),
            geometry,
            CommitCounter::default(),
            WALL_FILL,
            Kind::Unspecified,
        )));
    }
    if !backdrop_done {
        backdrop_element(plan, origin, out);
    }
}

/// The colour behind the stage's frames (`backdrop-color`): the whole monitor, fading with the
/// overview. Nothing when it is not set or already faded out. Only the stage has one.
pub(super) fn backdrop_element(
    plan: &OverviewPlan,
    origin: Point<i32, Logical>,
    out: &mut Vec<JffElement>,
) {
    let Some(([r, g, b, a], id)) = &plan.backdrop else {
        return;
    };
    if !plan.wall.as_ref().is_some_and(|wall| wall.behind) {
        return;
    }
    let a = a * plan.cell_alpha.clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let monitor = output_rect(&plan.output);
    out.push(JffElement::Solid(SolidColorRenderElement::new(
        id.clone(),
        Rectangle::<i32, Physical>::new(
            (monitor.0 - origin.x, monitor.1 - origin.y).into(),
            (monitor.2, monitor.3).into(),
        ),
        CommitCounter::default(),
        // Solid colours are premultiplied.
        [r * a, g * a, b * a, a],
        Kind::Unspecified,
    )));
}

/// How much of the rim frame `i` of the stage gets: the first one is the shown
/// workspace, the rest are its neighbours.
pub(super) fn stage_strength(i: usize) -> f32 {
    if i == 0 {
        1.0
    } else {
        STAGE_PEEK_STRENGTH
    }
}

/// The monitor's wallpaper shrunk into every square of the wall, with the square's rounded
/// corners. Behind the windows of the square and in front of its fill.
pub(super) fn wallpaper_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    wall: &WallPlan,
    origin: Point<i32, Logical>,
    out: &mut Vec<JffElement>,
) {
    if wall.wallpaper.is_empty() {
        return;
    }
    let monitor = output_rect(&plan.output);
    // The mask only shapes what is inside the frame's rectangle: what an element draws outside
    // it is left as it is, not cut off. Scaled to the width alone, rounding could make the
    // wallpaper a pixel taller than the frame, and that extra row showed as a line along the
    // edge that the rounded corners then leave sticking out. So it is made to fit inside the
    // frame, whichever side is the tight one, and centered in it.
    let (cell_w, cell_h) = wall.cells.first().map_or((1, 1), |cell| (cell.2, cell.3));
    let scale = f64::min(
        f64::from((cell_w - 2).max(1)) / f64::from(monitor.2.max(1)),
        f64::from((cell_h - 2).max(1)) / f64::from(monitor.3.max(1)),
    );
    let inset_x = (f64::from(cell_w) - f64::from(monitor.2) * scale) / 2.0;
    let inset_y = (f64::from(cell_h) - f64::from(monitor.3) * scale) / 2.0;
    for cell in &wall.cells {
        let rounding = Rounding::new(
            renderer,
            Rectangle::new(
                (cell.0 - origin.x, cell.1 - origin.y).into(),
                (cell.2, cell.3).into(),
            ),
            wall.radius,
        );
        // Later surfaces of the layer are in front: the list is back to front, draw it so.
        for (surface, loc) in wall.wallpaper.iter().rev() {
            let frame = crate::render::effects::frame_scale::get();
            let x = f64::from(cell.0 - origin.x) + inset_x + f64::from(loc.x) * scale;
            let y = f64::from(cell.1 - origin.y) + inset_y + f64::from(loc.y) * scale;
            let slot: Point<i32, Physical> =
                Point::from(((x * frame).round() as i32, (y * frame).round() as i32));
            let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    &mut *renderer,
                    surface,
                    Point::<i32, Physical>::from((0, 0)),
                    Scale::from(crate::render::effects::frame_scale::get()),
                    1.0,
                    Kind::Unspecified,
                );
            let conversions = gamut::tree_conversions(surface);
            let alpha_modes = crate::render::effects::pixel_alpha::modes(surface);
            for element in surfaces {
                let conversion = gamut::find(&conversions, element.id());
                let alpha_mode =
                    crate::render::effects::pixel_alpha::find(&alpha_modes, element.id());
                let shrunk = RescaleRenderElement::from_element(
                    element,
                    Point::<i32, Physical>::from((0, 0)),
                    scale,
                );
                let placed = RelocateRenderElement::from_element(shrunk, slot, Relocate::Relative);
                out.push(
                    match gamut::masked(&mut *renderer, rounding.as_ref(), conversion, alpha_mode) {
                        Some(mask) => mask.wrap_thumb(placed),
                        None => JffElement::Thumb(placed),
                    },
                );
            }
        }
    }
}

/// Elements for the dragged thumbnail on a monitor that isn't showing the overview, so you can
/// see the window travelling there. `origin` is the monitor's top-left at `rect`.
pub fn overview_drag_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    origin: Point<i32, Logical>,
    rect: Rect,
    blur: Option<config::Blur>,
    snapshot: Option<&WallpaperSnapshot>,
) -> Vec<JffElement> {
    let mut out: Vec<JffElement> = Vec::new();
    // The carried window, and the rest of its container.
    for i in plan
        .dragged
        .into_iter()
        .chain(plan.dragged_with.iter().copied())
    {
        let Some((window, thumb)) = plan.thumbs.get(i) else {
            continue;
        };
        if layout::intersects(thumb.rect(), rect) {
            badge_elements(renderer, plan, i, origin, &mut out);
            thumb_elements(
                renderer,
                window,
                thumb,
                origin,
                plan.blur_windows.apply(blur, &app_id_of(window)),
                plan.radius,
                &mut out,
                snapshot,
            );
        }
    }
    out
}
